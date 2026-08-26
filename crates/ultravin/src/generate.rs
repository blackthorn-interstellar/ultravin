//! VIN generation from the embedded artifact — no database, no network.
//!
//! Everything the decoder reads to resolve a VIN is enough to *build* one: the
//! WMIs, their schemas and year bands, the patterns' `Keys` specs, and the
//! check-digit rules. So the same crate that decodes can hand out VINs to
//! exercise a decoder — yours or anyone else's — with nothing else installed.
//!
//! Three ways in, in increasing size:
//!
//! - [`generate`] — `n` valid VINs, deterministic per seed, optionally filtered
//!   to a WMI, make, model year (exact or a range) or vehicle type.
//! - [`Db::cover`] — the smallest set that exercises every decode behaviour the
//!   data can reach (computed when the artifact is built).
//! - [`sweep`] — one VIN per row of every dimension that can change a decode,
//!   plus the cases that are not rows at all. Hundreds of thousands of VINs.

use std::collections::BTreeSet;
use std::collections::HashSet;

use crate::db::Db;

/// Model-year characters for 2010..=2039 (I/O/Q and U/Z excluded, 30-year cycle).
const MY_CHARS: &[u8; 30] = b"ABCDEFGHJKLMNPRSTVWXY123456789";
/// VDS/plant fill; the serial gets digits so the check digit lands on a real VIN.
const FILL: u8 = b'A';
const FILL_SERIAL: u8 = b'1';
/// The default per-position class `[A-H,J-N,P,R-Z,0-9]` — every character legal
/// at an unconstrained VDS/plant position. Randomized fills draw from this.
const DEFAULT_CLASS: &[u8; 33] = b"ABCDEFGHJKLMNPRSTUVWXYZ0123456789";

/// Which VINs [`generate`] is allowed to return. Every field is a conjunct; an
/// unset field constrains nothing.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    /// Exact WMI (3 or 6 characters), case-insensitive.
    pub wmi: Option<String>,
    /// Make name, case-insensitive (e.g. "HONDA").
    pub make: Option<String>,
    /// Model year; only schemas covering it are used.
    pub year: Option<i32>,
    /// Lowest model year to accept (inclusive). Conjunct with `year`/`max_year`.
    pub min_year: Option<i32>,
    /// Highest model year to accept (inclusive). Conjunct with `year`/`min_year`.
    pub max_year: Option<i32>,
    /// `VehicleType` row id (2 = passenger car, 7 = MPV, ...).
    pub vehicle_type: Option<i32>,
}

/// The model-year character for a year, as position 10 encodes it.
pub fn year_char(year: i32) -> char {
    MY_CHARS[(year - 2010).rem_euclid(30) as usize] as char
}

/// Per-position characters a `Keys` spec allows: `*`/`_` anything, `[ABC]` and
/// `[A-C]` their members, `|` the VDS/VIS split, anything else a literal. A
/// reversed range like `[C-A]` yields nothing, exactly as the regex engine sees it.
///
/// With `rng`, positions the spec leaves open take a random satisfying character
/// (`#` any digit, a class any legal member) instead of the fixed lowest one.
fn key_positions(keys: &str, mut rng: Option<&mut Rng>) -> Vec<Option<u8>> {
    let b = keys.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'*' | b'_' => {
                out.push(None);
                i += 1;
            }
            b'#' => {
                // A Formula Pattern digit slot: any digit will do, and leaving it
                // literal would emit a character that is not legal in a VIN.
                out.push(Some(match rng.as_deref_mut() {
                    Some(r) => b'0' + r.below(10) as u8,
                    None => b'1',
                }));
                i += 1;
            }
            b'[' => match crate::keyspec::class_body(b, i) {
                Some((body, next)) => {
                    out.push(class_member(body, rng.as_deref_mut()));
                    i = next;
                }
                None => {
                    out.push(Some(b[i]));
                    i += 1;
                }
            },
            c => {
                out.push(Some(c));
                i += 1;
            }
        }
    }
    out
}

/// I, O and Q are not legal VIN characters and have no check-digit
/// transliteration; a VIN must never contain one.
fn is_ioq(c: u8) -> bool {
    matches!(c, b'I' | b'O' | b'Q')
}

/// The character a bracket class contributes: a member that is legal in a VIN
/// (I/O/Q excluded) — the lowest one without `rng`, a uniformly random one with.
/// If every member is I/O/Q, returns that illegal member so `build_vin` drops the
/// candidate rather than silently mismatching the pattern; `None` only when the
/// class has no members at all (e.g. a reversed range), which leaves the position
/// at its fill.
fn class_member(body: &[u8], rng: Option<&mut Rng>) -> Option<u8> {
    let mut legal: Vec<u8> = Vec::new();
    let mut any: Option<u8> = None;
    for (lo, hi) in crate::keyspec::class_ranges(body) {
        if lo > hi {
            continue; // a reversed range contributes nothing
        }
        any = Some(any.map_or(lo, |m| m.min(lo)));
        legal.extend((lo..=hi).filter(|&c| !is_ioq(c)));
    }
    match rng {
        _ if legal.is_empty() => any,
        Some(r) => Some(legal[r.below(legal.len())]),
        None => legal.iter().min().copied(),
    }
}

/// Build a 17-character VIN for `wmi` that satisfies `keys` at model year `year`,
/// with a correct check digit, or `None` when the WMI or a key would force an
/// I/O/Q character into it. Those are illegal in a VIN and have no check-digit
/// transliteration, so such a candidate is skipped rather than emitted malformed.
///
/// A 6-character (low-volume) WMI also fills positions 12-14, which is where
/// `fVinWMI` looks for the rest of it when position 3 is `9`.
///
/// Deterministic on purpose: every unpinned position takes a fixed fill, so one
/// `(wmi, keys, year)` triple is one VIN string. The corpora (`sweep`, `seeded`,
/// the cover) and their frozen answer keys depend on that. [`generate`] wants the
/// opposite — variety — and goes through [`build_vin_filled`] with an RNG.
pub fn build_vin(wmi: &str, keys: &str, year: i32) -> Option<String> {
    build_vin_filled(wmi, keys, year, None)
}

/// [`build_vin`] with the fills chosen by `rng` instead of fixed: unpinned VDS
/// and plant positions draw from the full default class, the serial draws random
/// digits, and a key's open choices (`#`, bracket classes) draw a random
/// satisfying member. Still a VIN that satisfies `keys` — the pins land on top of
/// the fills — and still a pure function of the RNG state, so [`generate`] stays
/// deterministic per seed.
fn build_vin_filled(wmi: &str, keys: &str, year: i32, mut rng: Option<&mut Rng>) -> Option<String> {
    let mut vin = [FILL; 17];
    match rng.as_deref_mut() {
        Some(r) => {
            // Only the positions a fill can actually reach: 4-8 (VDS) and 11
            // (plant). WMI, check digit and year char are stamped below.
            for i in [3, 4, 5, 6, 7, 10] {
                vin[i] = DEFAULT_CLASS[r.below(DEFAULT_CLASS.len())];
            }
            // The serial stays numeric: digits are valid at positions 12-17 under
            // every check-digit rule, letters only at some.
            for slot in &mut vin[11..17] {
                *slot = b'0' + r.below(10) as u8;
            }
        }
        None => vin[11..17].fill(FILL_SERIAL),
    }

    let w = wmi.as_bytes();
    for (i, &c) in w.iter().take(3).enumerate() {
        vin[i] = c;
    }
    if w.len() == 6 {
        for (i, &c) in w[3..6].iter().enumerate() {
            vin[11 + i] = c;
        }
    }
    vin[9] = year_char(year) as u8;

    let mut parts = keys.split('|');
    if let Some(vds) = parts.next() {
        for (i, ch) in key_positions(vds, rng.as_deref_mut())
            .into_iter()
            .take(5)
            .enumerate()
        {
            if let Some(c) = ch {
                vin[3 + i] = c;
            }
        }
    }
    if let Some(vis) = parts.next() {
        for (i, ch) in key_positions(vis, rng).into_iter().take(8).enumerate() {
            if let Some(c) = ch {
                vin[9 + i] = c;
            }
        }
    }

    // I/O/Q reached here only from the WMI bytes or a key literal/class; either
    // way this is not a VIN we can emit, so skip the candidate.
    if vin.iter().any(|&c| is_ioq(c)) {
        return None;
    }

    stamp_check_digit(&mut vin);
    // Lossy, not `from_utf8`: a WMI or key literal carrying a non-UTF-8 byte would
    // otherwise turn a VIN into an error, and this is the one copy that has to
    // happen anyway because the result is owned.
    Some(String::from_utf8_lossy(&vin).into_owned())
}

/// Stamp position 9 (index 8) with the computed check digit. The `'0'` fallback
/// still guards any shape `check_digit` rejects (a letter in a numeric-only
/// position); with I/O/Q gone it is not reached in practice, but it keeps the VIN
/// well formed rather than panicking.
pub(crate) fn stamp_check_digit(vin: &mut [u8]) {
    vin[8] = b'0';
    // The lossy view is borrowed, not owned: `check_digit` only reads it, and for
    // the ASCII case every real VIN takes that is no allocation at all. It has to
    // land in a local first so the borrow of `vin` ends before the write below.
    let digit = match crate::check_digit(&String::from_utf8_lossy(vin)) {
        Some(c) if c != '?' => c as u8,
        _ => b'0',
    };
    vin[8] = digit;
}

/// A model year inside a schema's band, preferring recent years.
pub(crate) fn pick_year(yearfrom: i32, yearto: i32, current_year: i32) -> i32 {
    let cap = current_year + 2;
    let hi = yearto.min(cap).min(2039);
    let lo = yearfrom.max(2010);
    if lo <= hi {
        hi
    } else if yearto < 2010 {
        yearto
    } else {
        yearfrom
    }
}

/// The same band as [`pick_year`], sampled instead of maximised.
///
/// Taking the cap is right for the corpora: `sweep`, `pairwise`, `seeded` and the
/// cover must be byte-identical run to run, and the cover's contents are hashed
/// into the shipped artifact. It is wrong for `generate`, whose VINs are a random
/// sample of the data — pinning the year puts ~83% of them on `current_year + 2`,
/// so a fixture exercises one model-year row of each schema instead of the band
/// the schema actually spans. Bands too old to express (the `lo > hi` arms) have
/// exactly one answer, so they defer.
fn sample_year(rng: &mut Rng, yearfrom: i32, yearto: i32, current_year: i32) -> i32 {
    let hi = yearto.min(current_year + 2).min(2039);
    let lo = yearfrom.max(2010);
    if lo <= hi {
        lo + rng.below((hi - lo + 1) as usize) as i32
    } else {
        pick_year(yearfrom, yearto, current_year)
    }
}

fn year_to(link: &crate::tables::ArchivedWmiVinSchema) -> i32 {
    link.yearto_or(9999)
}

/// SplitMix64 — a deterministic, dependency-free PRNG. Generation must repeat
/// exactly for a given seed or a corpus is not a fixture.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Does `vin` carry the check digit the decoder will compute for it?
///
/// A pattern's `Keys` can pin a letter where the VIN standard demands a digit
/// (positions 15-17 always, 13-14 for a 3-character WMI, and 13 for a car/MPV/
/// light truck). `fVINCheckDigit2` then returns no digit at all, `build_vin`
/// stamps its '0' fallback, and the VIN decodes to error 400 plus a check-digit
/// failure — the opposite of what [`generate`] promises. About 8 of the 1.65M
/// (schema, keys) combinations do this, so which seeds trip over one is luck.
///
/// The car/MPV/LT flag comes from `wmi_any` on the VIN's own descriptor WMI
/// because that is where `compute_errors` gets it. Taking it from the row this
/// candidate was drawn from would agree today and stop agreeing the month two
/// rows share a WMI string — the case `wmi_by_str` already loops for.
fn check_digit_agrees(db: &Db, vin: &str) -> bool {
    let car_lt = db
        .wmi_any(&crate::vin_wmi(vin))
        .is_some_and(|w| w.is_car_mpv_lt());
    crate::checkdigit::check_digit_with_flag(vin, car_lt) == Some(vin.as_bytes()[8] as char)
}

/// `n` valid VINs matching `filter`, deterministic for a given `seed`.
///
/// Each VIN is drawn from a real WMI, a schema that WMI actually uses, and one of
/// that schema's patterns, so it decodes to real vehicle attributes rather than
/// to an unknown-manufacturer error. Returns fewer than `n` when the filter
/// matches nothing — including a `wmi` that exists in the data but is not
/// published yet, which the decoder refuses to resolve and this refuses to emit.
///
/// Every position a pattern leaves open is randomized — the serial digits, the
/// unpinned VDS/plant positions, and the free choices inside a key (`#`, bracket
/// classes) — so two draws of the same (WMI, schema, pattern) still yield
/// different strings essentially always. **The result can still repeat a VIN**
/// (the draws are independent, nothing dedups), but collisions need identical
/// random fills on top of an identical combo, so they are vanishingly rare
/// rather than the norm they were when the fills were fixed. Deduplicating here
/// would silently return fewer than `n`, so the caller decides — take the odds,
/// or use [`seeded`], the deterministic deduplicated corpus builder.
///
/// The clock is an argument, not a reading: the result is a pure function of
/// (`n`, `seed`, `filter`, `now_micros`, `current_year`) over a given artifact,
/// so a fixture repeats exactly. `now_micros` and `current_year` are the same
/// pair [`crate::decode_full`] takes, and must be, because a `filter.year` or
/// `filter.make` is checked by decoding the candidate (see below).
pub fn generate(
    db: &Db,
    n: usize,
    seed: u64,
    filter: &Filter,
    now_micros: i64,
    current_year: i32,
) -> Vec<String> {
    let makeids = filter
        .make
        .as_deref()
        .map(|m| db.lookup_ids_by_name(crate::tables::element_lookup_tag(26).unwrap_or(0), m));

    let candidates: Vec<&crate::tables::ArchivedWmi> = db
        .wmis()
        .iter()
        // A WMI the decoder will not resolve cannot appear in a corpus that
        // promises decodable VINs: `decode_full` goes through `wmi_by_str`, which
        // skips rows whose public-availability date is NULL or still in the future,
        // and reports the miss as error 7. Same clock, same predicate, so a
        // generated VIN's manufacturer is registered by construction. The corpora
        // built for the answer key (`seeded`, `sweep`, `pairwise`, the cover) draw
        // from the raw list on purpose — unregistered WMIs are how they reach the
        // error paths.
        .filter(|w| w.is_public(now_micros))
        .filter(|w| match filter.wmi.as_deref() {
            Some(want) => db.s(w.wmi.to_native()).eq_ignore_ascii_case(want),
            None => true,
        })
        .filter(|w| match filter.vehicle_type {
            Some(vt) => w.vehicletypeid.to_native() == vt,
            None => true,
        })
        .filter(|w| match makeids.as_deref() {
            Some(ids) => db
                .wmi_makes_for(w.id.to_native())
                .iter()
                .any(|m| ids.contains(&m.makeid.to_native())),
            None => true,
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    // The three year fields collapse to one inclusive range: `year` is a
    // one-year range, and all of them are conjuncts. An empty intersection
    // (min > max, or a `year` outside the range) matches nothing.
    let year_lo = filter
        .min_year
        .unwrap_or(i32::MIN)
        .max(filter.year.unwrap_or(i32::MIN));
    let year_hi = filter
        .max_year
        .unwrap_or(i32::MAX)
        .min(filter.year.unwrap_or(i32::MAX));
    if year_lo > year_hi {
        return Vec::new();
    }
    let year_constrained = year_lo != i32::MIN || year_hi != i32::MAX;

    let mut rng = Rng(seed);
    // `n` is caller-supplied; a huge value would abort in the allocator before the
    // attempt bound can limit the work. The Vec grows as needed, so only cap the
    // starting capacity. The Python boundary rejects absurd `n` outright.
    let mut out = Vec::with_capacity(n.min(65_536));
    // Bounded twice over. The attempt cap stops a filtered WMI set whose WMIs have
    // no usable schema from spinning forever. `misses` stops the other shape: a
    // filter that is satisfiable in principle and essentially never in practice —
    // `year` costs a decode per candidate, so at the documented 10M ceiling the
    // attempt cap alone is hours of work to hand back an empty Vec. A run this long
    // with no hit since means the filter is starving, and returning early says the
    // same thing sooner: fewer than `n` because the filter matched nothing.
    const STARVATION: usize = 4096;
    let mut attempts = 0usize;
    let mut misses = 0usize;
    while out.len() < n && attempts < n.saturating_mul(64).max(4096) && misses < STARVATION {
        attempts += 1;
        misses += 1; // cleared by the push below; every `continue` is a miss
        let w = candidates[rng.below(candidates.len())];
        let all = db.wmi_vinschema_for(w.id.to_native());
        // Unfiltered, every link qualifies, so draw straight from the slice; the
        // constrained arm is the only one that needs a narrowed list to draw
        // from — links whose band intersects the requested range.
        let link = if year_constrained {
            let links: Vec<_> = all
                .iter()
                .filter(|l| year_hi >= l.yearfrom.to_native() && year_lo <= year_to(l))
                .collect();
            if links.is_empty() {
                continue;
            }
            links[rng.below(links.len())]
        } else {
            if all.is_empty() {
                continue;
            }
            &all[rng.below(all.len())]
        };
        let year = match filter.year {
            Some(y) => y,
            // The link's band clamped to the requested range is never inverted:
            // the link filter above only kept bands that intersect it.
            None => sample_year(
                &mut rng,
                link.yearfrom.to_native().max(year_lo),
                year_to(link).min(year_hi),
                current_year,
            ),
        };

        let patterns = db.patterns_for(link.vinschemaid.to_native());
        let keys = if patterns.is_empty() {
            "*****"
        } else {
            db.s(patterns[rng.below(patterns.len())].keys.to_native())
        };
        let Some(vin) = build_vin_filled(db.s(w.wmi.to_native()), keys, year, Some(&mut rng))
        else {
            continue;
        };
        if !check_digit_agrees(db, &vin) {
            continue;
        }
        // The year range and `make` are promises about the *decoded* VIN, and
        // the draw alone cannot keep either. Year: position 10 is a 30-year cycle, so `L`
        // is both 2020 and 1990; `fVinModelYear2` only resolves that from the VIN
        // when the WMI is a car/MPV/light truck (position 7 then picks the half),
        // everywhere else both halves get a decode pass and the best-of scoring —
        // not this function — decides which one the caller sees. Make: a WMI can
        // be linked to several makes (Honda's WMIs also carry Acura), and which
        // one a VIN carries is decided by the pattern its VDS characters match.
        // So the only honest filter is to decode the candidate and keep it when
        // it really resolves to what was asked for; the WMI/schema it was drawn
        // from is a necessary condition, not a sufficient one.
        if year_constrained || filter.make.is_some() {
            let full = crate::decode_full(db, &vin, now_micros, current_year, None);
            if year_constrained {
                match full.model_year {
                    Some(y) if (year_lo..=year_hi).contains(&y) => {}
                    _ => continue,
                }
            }
            if let Some(want) = filter.make.as_deref() {
                // The same equality `lookup_ids_by_name` used to pre-filter the
                // WMIs; element 26 is Make, its `value` the resolved lookup name.
                if !full
                    .elements
                    .iter()
                    .any(|e| e.element_id == 26 && e.value.eq_ignore_ascii_case(want))
                {
                    continue;
                }
            }
        }
        out.push(vin);
        misses = 0;
    }
    out
}

/// A slice of the data that a generated VIN can exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    /// One VIN per WMI.
    Wmi,
    /// One VIN per distinct (schema, keys) pattern — by far the largest.
    Pattern,
    /// One VIN per engine model reachable through an element-18 pattern.
    Engine,
    /// One VIN per vehicle-spec schema.
    VehicleSpec,
    /// Every VinException VIN verbatim; the only route into check-digit exemption.
    Exception,
    /// One VIN per vehicle type that has DefaultValue rows.
    Default,
}

impl Dimension {
    /// Every dimension, in the order [`sweep`] emits them.
    pub const ALL: [Dimension; 6] = [
        Dimension::Wmi,
        Dimension::Pattern,
        Dimension::Engine,
        Dimension::VehicleSpec,
        Dimension::Exception,
        Dimension::Default,
    ];
}

/// One VIN per row of each requested dimension.
///
/// This is the brute-force list: exhaustive over the data, and correspondingly
/// large — the `Pattern` dimension alone is ~545k VINs. It cannot cover cases
/// that are not rows in any table (deliberate ties, model-year edges, error
/// codes); those live in the cover, which is built from this plus constructions.
pub fn sweep(db: &Db, dimensions: &[Dimension], current_year: i32) -> Vec<String> {
    let mut out = Vec::new();
    let index = WmiIndex::build(db);
    for dim in dimensions {
        match dim {
            Dimension::Wmi => {
                for w in db.wmis() {
                    let links = db.wmi_vinschema_for(w.id.to_native());
                    let year = links.first().map_or(current_year, |l| {
                        pick_year(l.yearfrom.to_native(), year_to(l), current_year)
                    });
                    out.extend(build_vin(db.s(w.wmi.to_native()), "*****", year));
                }
            }
            Dimension::Pattern => sweep_patterns(db, &index, current_year, &mut out),
            Dimension::Engine => sweep_engines(db, &index, current_year, &mut out),
            Dimension::VehicleSpec => sweep_vspecs(db, &index, current_year, &mut out),
            Dimension::Exception => {
                out.extend(
                    db.vinexceptions()
                        .iter()
                        .map(|e| db.s(e.vin.to_native()).to_string()),
                );
            }
            Dimension::Default => sweep_defaults(db, current_year, &mut out),
        }
    }
    out
}

/// The two id-keyed views of the WMI table every corpus builder needs.
///
/// Both are a full scan of `wmi` plus a sort, and every builder needs the same
/// two, so they are built once at the entry point and passed down. Rebuilding
/// per helper cost `sweep(ALL)` six constructions of the identical data.
pub(crate) struct WmiIndex<'a> {
    /// One `(schema id, WMI id, yearfrom, yearto)` per schema, in schema-id order.
    /// A pattern names only its schema, so this is what turns one into a VIN;
    /// `wmi_vinschema` is keyed the other way round.
    schemas: Vec<(i32, i32, i32, i32)>,
    /// WMI strings by row id. `wmi` is sorted by the WMI *string*, so a lookup by
    /// id is a scan; doing that per pattern is the difference between seconds and
    /// hours.
    by_id: Vec<(i32, &'a str)>,
}

impl<'a> WmiIndex<'a> {
    pub(crate) fn build(db: &'a Db) -> Self {
        let mut schemas: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut by_id: Vec<(i32, &str)> = Vec::with_capacity(db.wmis().len());
        for w in db.wmis() {
            let id = w.id.to_native();
            by_id.push((id, db.s(w.wmi.to_native())));
            for l in db.wmi_vinschema_for(id) {
                schemas.push((
                    l.vinschemaid.to_native(),
                    id,
                    l.yearfrom.to_native(),
                    year_to(l),
                ));
            }
        }
        // Sorted on the whole tuple, so which WMI a schema keeps is a property of
        // the data rather than of the scan order, and the corpora stay stable.
        schemas.sort_unstable();
        schemas.dedup_by_key(|p| p.0);
        by_id.sort_unstable_by_key(|e| e.0);
        Self { schemas, by_id }
    }

    /// Every `(schema, WMI, yearfrom, yearto)` entry, in schema-id order.
    pub(crate) fn schemas(&self) -> &[(i32, i32, i32, i32)] {
        &self.schemas
    }

    /// The entry for one schema id.
    pub(crate) fn schema(&self, schemaid: i32) -> Option<(i32, i32, i32, i32)> {
        self.schemas
            .binary_search_by_key(&schemaid, |e| e.0)
            .ok()
            .map(|i| self.schemas[i])
    }

    /// The WMI string for a row id.
    pub(crate) fn wmi(&self, wmiid: i32) -> Option<&'a str> {
        self.by_id
            .binary_search_by_key(&wmiid, |e| e.0)
            .ok()
            .map(|i| self.by_id[i].1)
    }
}

fn sweep_patterns(db: &Db, index: &WmiIndex<'_>, current_year: i32, out: &mut Vec<String>) {
    // Patterns are grouped by schema but ordered by id within it, so identical
    // keys are not adjacent: dedup against the keys seen for the current schema,
    // and drop the set when the schema changes so this stays streaming.
    let mut schema = i32::MIN;
    let mut seen: Vec<u32> = Vec::new();
    for p in db.patterns() {
        let sid = p.vinschemaid.to_native();
        if sid != schema {
            schema = sid;
            seen.clear();
        }
        let key_id = p.keys.to_native();
        if let Err(pos) = seen.binary_search(&key_id) {
            seen.insert(pos, key_id);
        } else {
            continue;
        }
        let Some(entry) = index.schema(sid) else {
            continue;
        };
        if let Some(wmi) = index.wmi(entry.1) {
            out.extend(build_vin(
                wmi,
                db.s(key_id),
                pick_year(entry.2, entry.3, current_year),
            ));
        }
    }
}

/// Patterns for one element, indexed by their (normalised) attribute id.
/// Scanning all 1.6M patterns per row instead costs minutes.
fn patterns_by_attribute(db: &Db, elementid: i32, lower: bool) -> Vec<(String, usize)> {
    let mut index: Vec<(String, usize)> = db
        .patterns()
        .iter()
        .enumerate()
        .filter(|(_, p)| p.elementid.to_native() == elementid)
        .map(|(i, p)| {
            let attr = db.s(p.attributeid.to_native()).trim();
            (
                if lower {
                    attr.to_ascii_lowercase()
                } else {
                    attr.to_string()
                },
                i,
            )
        })
        .collect();
    index.sort_unstable();
    index.dedup_by(|a, b| a.0 == b.0);
    index
}

fn sweep_engines(db: &Db, index: &WmiIndex<'_>, current_year: i32, out: &mut Vec<String>) {
    let by_name = patterns_by_attribute(db, 18, true);
    for em in db.enginemodels() {
        let name = db.s(em.name.to_native()).trim().to_ascii_lowercase();
        let hit = by_name
            .binary_search_by(|probe| probe.0.as_str().cmp(name.as_str()))
            .ok()
            .map(|i| &db.patterns()[by_name[i].1]);
        let Some(p) = hit else { continue };
        let Some(entry) = index.schema(p.vinschemaid.to_native()) else {
            continue;
        };
        if let Some(wmi) = index.wmi(entry.1) {
            out.extend(build_vin(
                wmi,
                db.s(p.keys.to_native()),
                pick_year(entry.2, entry.3, current_year),
            ));
        }
    }
}

fn sweep_vspecs(db: &Db, index: &WmiIndex<'_>, current_year: i32, out: &mut Vec<String>) {
    let by_model = patterns_by_attribute(db, 28, false);
    for vs in db.vspecschemas() {
        let models = db.vspecschema_models_for(vs.id.to_native());
        let Some(m) = models.first() else { continue };
        let modelid = m.modelid.to_native().to_string();
        // A Model (element 28) pattern naming this model is the way in: the spec
        // rung matches on make/model/year, not on the VIN's own characters.
        let hit = by_model
            .binary_search_by(|probe| probe.0.as_str().cmp(modelid.as_str()))
            .ok()
            .map(|i| &db.patterns()[by_model[i].1]);
        let Some(p) = hit else { continue };
        let years = db.vspecschema_years_for(vs.id.to_native());
        let Some(entry) = index.schema(p.vinschemaid.to_native()) else {
            continue;
        };
        let year = years
            .iter()
            .map(|y| y.year.to_native())
            .find(|y| *y >= entry.2 && *y <= entry.3)
            .unwrap_or_else(|| pick_year(entry.2, entry.3, current_year));
        if let Some(wmi) = index.wmi(entry.1) {
            out.extend(build_vin(wmi, db.s(p.keys.to_native()), year));
        }
    }
}

fn sweep_defaults(db: &Db, current_year: i32, out: &mut Vec<String>) {
    let mut seen: Vec<i32> = Vec::new();
    for dv in db.defaultvalues() {
        let vt = dv.vehicletypeid.to_native();
        if seen.contains(&vt) {
            continue;
        }
        seen.push(vt);
        let hit = db.wmis().iter().find(|w| {
            w.vehicletypeid.to_native() == vt && !db.wmi_vinschema_for(w.id.to_native()).is_empty()
        });
        if let Some(w) = hit {
            let l = &db.wmi_vinschema_for(w.id.to_native())[0];
            out.extend(build_vin(
                db.s(w.wmi.to_native()),
                "*****",
                pick_year(l.yearfrom.to_native(), year_to(l), current_year),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_chars_cycle_every_thirty_years() {
        assert_eq!(year_char(2010), 'A');
        assert_eq!(year_char(2020), 'L');
        assert_eq!(year_char(2039), '9');
        assert_eq!(year_char(2040), year_char(2010));
        assert_eq!(year_char(1995), year_char(2025));
    }

    #[test]
    fn pick_year_takes_the_newest_year_the_band_and_the_clock_both_allow() {
        // Inside an open band the answer is the cap, not the band's own end.
        assert_eq!(pick_year(2015, 9999, 2026), 2028);
        // Whichever of the band and the clock binds first wins.
        assert_eq!(pick_year(2015, 2020, 2026), 2020);
        assert_eq!(pick_year(2015, 2028, 2026), 2028);
        // Position 10 is a 30-year cycle starting at 2010, so 2039 is the newest
        // year expressible at all however far the clock has run.
        assert_eq!(pick_year(2015, 9999, 2099), 2039);
        // Bands the cycle cannot express have exactly one answer each: wholly in
        // the past, take `yearto`; starting after the cap, take `yearfrom`.
        assert_eq!(pick_year(1995, 1998, 2026), 1998);
        assert_eq!(pick_year(2035, 9999, 2026), 2035);
        // A band opening before 2010 still starts at 2010 for the `lo <= hi` test.
        assert_eq!(pick_year(1995, 2015, 2026), 2015);
    }

    #[test]
    fn sampling_stays_inside_the_band_the_max_would_have_picked() {
        let mut rng = Rng(1);
        for _ in 0..200 {
            let y = sample_year(&mut rng, 2015, 9999, 2026);
            assert!((2015..=2028).contains(&y), "{y} left the band");
        }
        // A one-year band samples that year; a band ending before 2010 cannot be
        // expressed in position 10 at all, so it defers to `pick_year`.
        assert_eq!(sample_year(&mut rng, 2028, 2028, 2026), 2028);
        assert_eq!(sample_year(&mut rng, 1995, 1998, 2026), 1998);
        assert_eq!(pick_year(1995, 1998, 2026), 1998);
    }

    #[test]
    fn a_letter_in_a_numeric_only_position_fails_the_check_digit_gate() {
        let Some(db) = Db::try_embedded() else {
            eprintln!("skip: artifact not built");
            return;
        };
        assert!(
            db.wmi_any("1HG").is_some_and(|w| w.is_car_mpv_lt()),
            "1HG is the passenger-car WMI this test's position-13 rule needs"
        );
        assert!(check_digit_agrees(
            db,
            &build_vin("1HG", "CM826", 2026).unwrap()
        ));
        // VIS index 4 is VIN position 14: numeric-only for any 3-character WMI.
        // This is the real shape that leaked out — WMI 4BE with a `TAT` key tail.
        assert!(!check_digit_agrees(
            db,
            &build_vin("1HG", "CM826|****T", 2026).unwrap()
        ));
        // VIS index 3 is position 13: numeric-only *only* for a car/MPV/LT, so
        // this case also pins which flag the gate passes to `fVINCheckDigit2`.
        assert!(!check_digit_agrees(
            db,
            &build_vin("1HG", "CM826|***T", 2026).unwrap()
        ));
    }

    #[test]
    fn generation_never_draws_a_wmi_the_decoder_will_not_resolve() {
        let Some(db) = Db::try_embedded() else {
            eprintln!("skip: artifact not built");
            return;
        };
        let now = crate::now_micros();
        // A WMI with a schema (so it could be generated from) whose every row is
        // unpublished (so `wmi_by_str` returns nothing and a decode is error 7).
        let hit = db.wmis().iter().find(|w| {
            !db.wmi_vinschema_for(w.id.to_native()).is_empty()
                && db.wmi_by_str(db.s(w.wmi.to_native()), now).is_none()
        });
        let Some(w) = hit else {
            eprintln!("skip: every WMI in this data month is published");
            return;
        };
        let wmi = db.s(w.wmi.to_native()).to_string();
        let filter = Filter {
            wmi: Some(wmi.clone()),
            ..Default::default()
        };
        assert!(
            generate(db, 10, 1, &filter, now, crate::current_year()).is_empty(),
            "{wmi} is unpublished but still generated VINs"
        );
    }

    #[test]
    fn built_vins_are_well_formed() {
        let vin = build_vin("1HG", "CM826", 2003).unwrap();
        assert_eq!(vin.len(), 17);
        assert!(vin.starts_with("1HGCM826"));
        assert_eq!(crate::check_digit(&vin), vin.chars().nth(8));
    }

    #[test]
    fn formula_digit_slots_do_not_leak_into_the_vin() {
        // '#' is a digit slot, not a literal; left alone it makes a non-VIN.
        let vin = build_vin("1HG", "A#B*C", 2020).unwrap();
        assert_eq!(vin.len(), 17);
        assert!(vin
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
        assert!(!vin.contains('#'));
    }

    #[test]
    fn bracket_classes_pick_a_member_and_ranges_expand() {
        assert_eq!(
            build_vin("1HG", "[CD]M826", 2020).unwrap()[3..8].to_string(),
            "CM826"
        );
        assert_eq!(
            build_vin("1HG", "[C-F]M826", 2020).unwrap()[3..8].to_string(),
            "CM826"
        );
        // A reversed range accepts nothing, so the position keeps its fill.
        assert_eq!(
            build_vin("1HG", "[F-C]M826", 2020).unwrap()[3..5].to_string(),
            "AM"
        );
    }

    #[test]
    fn io_q_is_never_emitted() {
        // A WMI carrying I/O/Q cannot yield a legal VIN, so the candidate is
        // dropped rather than emitted malformed.
        assert!(build_vin("1OG", "*****", 2020).is_none());
        // A key literal demanding I/O/Q, likewise.
        assert!(build_vin("1HG", "IM826", 2020).is_none());
        // A class admitting I/O/Q *and* legal characters remaps to the lowest
        // legal one ([I-Z] -> J) instead of skipping.
        assert_eq!(
            build_vin("1HG", "[I-Z]M826", 2020).unwrap()[3..8].to_string(),
            "JM826"
        );
        // A class admitting only I/O/Q is dropped.
        assert!(build_vin("1HG", "****[OQ]", 2020).is_none());
    }

    #[test]
    fn six_char_wmis_fill_the_second_block() {
        let vin = build_vin("1G9ABC", "*****", 2020).unwrap();
        assert!(vin.starts_with("1G9"));
        assert_eq!(&vin[11..14], "ABC");
    }
}

// --------------------------------------------------------------------------- #
// Pairwise coverage
// --------------------------------------------------------------------------- #

/// The alphabet a VIN position can hold.
const ALPHABET: &[u8; 33] = b"ABCDEFGHJKLMNPRSTUVWXYZ0123456789";

/// Per position, one representative character per *equivalence class*.
///
/// Two characters are equivalent at a position when exactly the same set of the
/// schema's keys accept both — no pattern can tell them apart, so no VIN built
/// from one differs from the same VIN built from the other. Collapsing to
/// representatives turns 33 characters per position into a handful.
fn position_classes(db: &Db, schema: i32, pos3: u8, car_lt: bool) -> Vec<Vec<u8>> {
    let keys: Vec<&str> = db
        .patterns_for(schema)
        .iter()
        .map(|p| db.s(p.keys.to_native()))
        .collect();

    VARYING
        .iter()
        .map(|&(vin_pos, var_idx)| {
            // Only characters the VIN standard permits here: positions 14-17 are
            // numeric, and a car/MPV/light truck also has a numeric position 13.
            // Filling them with letters produces error 400, not vehicle variety.
            let legal: Vec<u8> = ALPHABET
                .iter()
                .copied()
                .filter(|&c| crate::checkdigit::valid_at(vin_pos, c, pos3, car_lt))
                .collect();
            // Signature of a character: which keys accept it here.
            let mut by_sig: Vec<(Vec<u16>, u8)> = Vec::new();
            for ch in legal {
                let mut sig: Vec<u16> = Vec::new();
                for (ki, k) in keys.iter().enumerate() {
                    if accepts_at(k, var_idx, ch) {
                        sig.push(ki as u16);
                    }
                }
                if !by_sig.iter().any(|(s, _)| *s == sig) {
                    by_sig.push((sig, ch));
                }
            }
            by_sig.into_iter().map(|(_, ch)| ch).collect()
        })
        .collect()
}

/// The descriptor positions pairwise varies, as (1-based VIN position, index into
/// `var_keys`). Position 10 (the model year) is excluded: varying it moves the
/// VIN out of the schema's own year band, which tests year resolution rather
/// than pattern interaction. `var_keys` index 5 is the `|` divider.
const VARYING: [(usize, usize); 12] = [
    (4, 0),
    (5, 1),
    (6, 2),
    (7, 3),
    (8, 4),
    (11, 7),
    (12, 8),
    (13, 9),
    (14, 10),
    (15, 11),
    (16, 12),
    (17, 13),
];

/// Does `keys` accept `ch` at var_keys position `pos`? A position the spec does
/// not reach, or leaves as `*`, accepts everything.
fn accepts_at(keys: &str, pos: usize, ch: u8) -> bool {
    let b = keys.as_bytes();
    let (mut i, mut p) = (0usize, 0usize);
    while i < b.len() {
        let (accepts, next) = match b[i] {
            b'*' | b'_' => (true, i + 1),
            b'[' => match crate::keyspec::class_body(b, i) {
                Some((body, next)) => (crate::keyspec::class_contains(body, ch), next),
                None => (b[i] == ch, i + 1),
            },
            c => (c == ch, i + 1),
        };
        if p == pos {
            return accepts;
        }
        i = next;
        p += 1;
    }
    true
}

/// A strength-2 covering array over `levels`: every pair of (position, class)
/// choices appears in at least one row.
///
/// Greedy row-at-a-time (AETG-style): seed each row with a still-uncovered pair,
/// then fill the remaining positions with whichever class covers the most
/// outstanding pairs. Greedy lands near the `v_max1 * v_max2` lower bound, and
/// unlike the full cartesian it terminates.
fn covering_array(levels: &[usize]) -> Vec<Vec<usize>> {
    let n = levels.len();
    // Ordered, not hashed: HashSet iteration is randomized per process, so a
    // hashed set here would make the corpus differ between runs — and an answer
    // key built on one machine would not verify on another.
    let mut uncovered: BTreeSet<(usize, usize, usize, usize)> = BTreeSet::new();
    for i in 0..n {
        for j in (i + 1)..n {
            for a in 0..levels[i] {
                for b in 0..levels[j] {
                    uncovered.insert((i, a, j, b));
                }
            }
        }
    }
    if uncovered.is_empty() {
        return vec![vec![0; n]];
    }

    let mut rows = Vec::new();
    while let Some(&(p1, c1, p2, c2)) = uncovered.iter().next() {
        let mut row = vec![usize::MAX; n];
        row[p1] = c1;
        row[p2] = c2;
        for p in 0..n {
            if row[p] != usize::MAX {
                continue;
            }
            // The class at `p` that covers the most pairs against what is set.
            let best = (0..levels[p])
                .max_by_key(|&c| {
                    (0..n)
                        .filter(|&q| q != p && row[q] != usize::MAX)
                        .filter(|&q| {
                            let key = if p < q {
                                (p, c, q, row[q])
                            } else {
                                (q, row[q], p, c)
                            };
                            uncovered.contains(&key)
                        })
                        .count()
                })
                .unwrap_or(0);
            row[p] = best;
        }
        for i in 0..n {
            for j in (i + 1)..n {
                uncovered.remove(&(i, row[i], j, row[j]));
            }
        }
        rows.push(row);
    }
    rows
}

/// VINs covering every pair of character-equivalence classes each schema can
/// distinguish — the strongest coverage that is actually finite.
///
/// The full output space cannot be enumerated: elements driven by disjoint
/// descriptor positions vary independently, so their values multiply. Strength 2
/// buys the interactions where the decoder's own logic lives (dedup, tiebreaks,
/// an element read while resolving a sibling) at roughly 3x the row sweep.
///
/// `limit` caps the result at that many VINs (0 = the lot); the full run is
/// ~1.7M VINs and minutes of work, which is more than a caller wanting a taste
/// needs.
pub fn pairwise(db: &Db, current_year: i32, limit: usize) -> Vec<String> {
    let index = WmiIndex::build(db);
    let mut out = Vec::new();
    for &(schema, wmiid, yearfrom, yearto) in index.schemas() {
        if limit > 0 && out.len() >= limit {
            break;
        }
        let Some(wmi) = index.wmi(wmiid) else {
            continue;
        };
        let wb = wmi.as_bytes();
        let pos3 = *wb.get(2).unwrap_or(&b'A');
        let car_lt = db.wmi_any(wmi).map(|w| w.is_car_mpv_lt()).unwrap_or(false);
        let classes = position_classes(db, schema, pos3, car_lt);
        let levels: Vec<usize> = classes.iter().map(|c| c.len()).collect();
        let year = pick_year(yearfrom, yearto, current_year);
        for row in covering_array(&levels) {
            // Lay the row back out over var_keys, keeping `|` at index 5 and the
            // model-year character at index 6 free for `build_vin` to set.
            let mut keys = [b'*'; 14];
            keys[5] = b'|';
            for (slot, &c) in row.iter().enumerate() {
                keys[VARYING[slot].1] = classes[slot][c];
            }
            out.extend(build_vin(
                wmi,
                std::str::from_utf8(&keys).unwrap_or("*****"),
                year,
            ));
        }
    }
    if limit > 0 {
        out.truncate(limit);
    }
    out
}

#[cfg(test)]
mod pairwise_tests {
    use super::*;

    fn covers_all_pairs(levels: &[usize], rows: &[Vec<usize>]) -> bool {
        for i in 0..levels.len() {
            for j in (i + 1)..levels.len() {
                for a in 0..levels[i] {
                    for b in 0..levels[j] {
                        if !rows.iter().any(|r| r[i] == a && r[j] == b) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    #[test]
    fn covering_array_covers_every_pair() {
        for levels in [vec![2, 2, 2], vec![3, 2, 4, 2], vec![5, 4, 3, 2, 2, 1]] {
            let rows = covering_array(&levels);
            assert!(
                covers_all_pairs(&levels, &rows),
                "missed a pair for {levels:?}"
            );
        }
    }

    #[test]
    fn covering_array_stays_near_the_lower_bound() {
        // No construction can beat v_max1 * v_max2; greedy should not be far off.
        let levels = vec![4, 3, 3, 2, 2];
        let rows = covering_array(&levels);
        assert!(rows.len() >= 12, "below the mathematical floor");
        assert!(rows.len() <= 24, "greedy drifted too far: {}", rows.len());
    }

    #[test]
    fn a_single_level_everywhere_needs_one_row() {
        assert_eq!(covering_array(&[1, 1, 1]).len(), 1);
    }

    #[test]
    fn class_membership_matches_the_keys_language() {
        assert!(accepts_at("CM826", 0, b'C'));
        assert!(!accepts_at("CM826", 0, b'D'));
        assert!(accepts_at("*M826", 0, b'D')); // wildcard
        assert!(accepts_at("CM826", 9, b'Z')); // beyond the spec: unconstrained
        assert!(accepts_at("[C-F]M", 0, b'E'));
        assert!(!accepts_at("[C-F]M", 0, b'G'));
    }
}

// --------------------------------------------------------------------------- #
// Seeded coverage: every rule matched, every class pair covered, one corpus
// --------------------------------------------------------------------------- #

/// Which classes at each position a rule accepts. `None` means it accepts every
/// class there — a free position the fill is allowed to choose.
fn seed_row(keys: &str, classes: &[Vec<u8>]) -> Vec<Option<usize>> {
    VARYING
        .iter()
        .enumerate()
        .map(|(slot, &(_, var_idx))| {
            let accepted: Vec<usize> = (0..classes[slot].len())
                .filter(|&c| accepts_at(keys, var_idx, classes[slot][c]))
                .collect();
            // Accepts everything here, or nothing we can honour: leave it free.
            if accepted.len() == classes[slot].len() || accepted.is_empty() {
                None
            } else {
                Some(accepted[0])
            }
        })
        .collect()
}

/// Pairs still uncovered, as a flat set keyed by (position, class, position, class).
fn all_pairs(levels: &[usize]) -> BTreeSet<(usize, usize, usize, usize)> {
    // Ordered: see covering_array — the corpus must be byte-identical run to run.
    let mut out = BTreeSet::new();
    for i in 0..levels.len() {
        for j in (i + 1)..levels.len() {
            for a in 0..levels[i] {
                for b in 0..levels[j] {
                    out.insert((i, a, j, b));
                }
            }
        }
    }
    out
}

/// Fill the free positions of `row` to cover as many outstanding pairs as
/// possible, then retire everything the finished row covers.
fn fill_and_retire(
    row: &mut [Option<usize>],
    levels: &[usize],
    uncovered: &mut BTreeSet<(usize, usize, usize, usize)>,
) -> Vec<usize> {
    for p in 0..row.len() {
        if row[p].is_some() {
            continue;
        }
        let best = (0..levels[p])
            .max_by_key(|&c| {
                (0..row.len())
                    .filter(|&q| q != p)
                    .filter_map(|q| row[q].map(|v| (q, v)))
                    .filter(|&(q, v)| {
                        let key = if p < q { (p, c, q, v) } else { (q, v, p, c) };
                        uncovered.contains(&key)
                    })
                    .count()
            })
            .unwrap_or(0);
        row[p] = Some(best);
    }
    let final_row: Vec<usize> = row.iter().map(|c| c.unwrap_or(0)).collect();
    for i in 0..final_row.len() {
        for j in (i + 1)..final_row.len() {
            uncovered.remove(&(i, final_row[i], j, final_row[j]));
        }
    }
    final_row
}

/// One corpus that does both jobs: every decoding rule is matched by some VIN,
/// **and** every pair of character-classes any two positions can distinguish
/// appears together.
///
/// Built the other way round from a plain covering array. Each rule's `Keys` is
/// a seed — the positions it pins stay pinned, so the rule is guaranteed to
/// match — and only the positions it leaves free are chosen to knock out
/// outstanding pairs. Sweeping and pairwise separately costs ~2.26M VINs and
/// spends half of them on fill characters chosen so that *nothing else* matches;
/// seeding costs ~1.75M and spends those same positions making sibling rules
/// co-match, which is where the tiebreak logic lives.
pub fn seeded(db: &Db, current_year: i32, limit: usize) -> Vec<String> {
    let index = WmiIndex::build(db);
    let mut out = Vec::new();
    // Two schemas can generate the same VIN string (filler-heavy rows collide);
    // emit each unique VIN once so the answer key / equivalence compare don't
    // see duplicate rows. First occurrence wins, so order stays deterministic.
    let mut seen: HashSet<String> = HashSet::new();

    for &(schema, wmiid, yearfrom, yearto) in index.schemas() {
        if limit > 0 && out.len() >= limit {
            break;
        }
        let Some(wmi) = index.wmi(wmiid) else {
            continue;
        };
        let wb = wmi.as_bytes();
        let pos3 = *wb.get(2).unwrap_or(&b'A');
        let car_lt = db.wmi_any(wmi).map(|w| w.is_car_mpv_lt()).unwrap_or(false);
        let classes = position_classes(db, schema, pos3, car_lt);
        let levels: Vec<usize> = classes.iter().map(|c| c.len()).collect();
        let year = pick_year(yearfrom, yearto, current_year);
        let mut uncovered = all_pairs(&levels);

        // One row per distinct rule, seeded and then filled.
        let mut seen_keys: Vec<u32> = Vec::new();
        for p in db.patterns_for(schema) {
            let key_id = p.keys.to_native();
            match seen_keys.binary_search(&key_id) {
                Ok(_) => continue,
                Err(pos) => seen_keys.insert(pos, key_id),
            }
            let mut row = seed_row(db.s(key_id), &classes);
            let filled = fill_and_retire(&mut row, &levels, &mut uncovered);
            if let Some(vin) = emit(wmi, &classes, &filled, year) {
                if seen.insert(vin.clone()) {
                    out.push(vin);
                }
            }
        }
        // Whatever the rules did not incidentally cover.
        while !uncovered.is_empty() {
            let &(p1, c1, p2, c2) = uncovered.iter().next().expect("non-empty");
            let mut row: Vec<Option<usize>> = vec![None; levels.len()];
            row[p1] = Some(c1);
            row[p2] = Some(c2);
            let filled = fill_and_retire(&mut row, &levels, &mut uncovered);
            if let Some(vin) = emit(wmi, &classes, &filled, year) {
                if seen.insert(vin.clone()) {
                    out.push(vin);
                }
            }
        }
    }
    if limit > 0 {
        out.truncate(limit);
    }
    out
}

/// A chosen class per position -> a VIN, or `None` if `build_vin` skips it.
fn emit(wmi: &str, classes: &[Vec<u8>], row: &[usize], year: i32) -> Option<String> {
    let mut keys = [b'*'; 14];
    keys[5] = b'|';
    for (slot, &c) in row.iter().enumerate() {
        keys[VARYING[slot].1] = classes[slot].get(c).copied().unwrap_or(b'A');
    }
    build_vin(wmi, std::str::from_utf8(&keys).unwrap_or("*****"), year)
}

#[cfg(test)]
mod seeded_tests {
    use super::*;

    #[test]
    fn a_seed_pins_only_what_its_rule_constrains() {
        // "CM826" pins all five VDS positions; the VIS positions stay free for
        // the fill to use on pair coverage.
        let classes: Vec<Vec<u8>> = (0..12).map(|_| b"ABC".to_vec()).collect();
        let row = seed_row("CM826", &classes);
        assert!(row[0].is_some(), "position 4 is pinned by the rule");
        assert!(row[5].is_none(), "position 11 is free");
    }

    #[test]
    fn a_wildcard_rule_pins_nothing() {
        let classes: Vec<Vec<u8>> = (0..12).map(|_| b"ABC".to_vec()).collect();
        assert!(seed_row("*****", &classes).iter().all(Option::is_none));
    }

    #[test]
    fn filling_retires_every_pair_the_row_covers() {
        let levels = vec![2, 2, 2];
        let mut uncovered = all_pairs(&levels);
        assert_eq!(uncovered.len(), 12); // 3 position pairs x 2 x 2
        let mut row = vec![Some(0), None, None];
        let filled = fill_and_retire(&mut row, &levels, &mut uncovered);
        assert_eq!(filled.len(), 3);
        // A 3-position row covers exactly its own 3 pairs.
        assert_eq!(uncovered.len(), 9);
    }

    #[test]
    fn seeding_then_closing_covers_every_pair() {
        let levels = vec![3, 2, 2];
        let mut uncovered = all_pairs(&levels);
        let mut rows = Vec::new();
        while !uncovered.is_empty() {
            let &(p1, c1, p2, c2) = uncovered.iter().next().unwrap();
            let mut row: Vec<Option<usize>> = vec![None; levels.len()];
            row[p1] = Some(c1);
            row[p2] = Some(c2);
            rows.push(fill_and_retire(&mut row, &levels, &mut uncovered));
        }
        for i in 0..levels.len() {
            for j in (i + 1)..levels.len() {
                for a in 0..levels[i] {
                    for b in 0..levels[j] {
                        assert!(
                            rows.iter().any(|r| r[i] == a && r[j] == b),
                            "pair ({i},{a})-({j},{b}) never appeared"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod corpus_tests {
    use super::*;

    /// The three public corpus builders take no seed: their whole contract is that
    /// the same artifact and year give the same list, because an answer key built
    /// on one machine has to verify on another.
    const YEAR: i32 = 2026;

    fn embedded() -> Option<&'static Db> {
        let db = Db::try_embedded();
        if db.is_none() {
            eprintln!("skip: artifact not built");
        }
        db
    }

    /// 17 characters, and none of them I/O/Q — the two things every VIN in every
    /// corpus must satisfy before anything else is worth asking.
    fn assert_well_formed(vins: &[String], what: &str) {
        assert!(!vins.is_empty(), "{what} produced nothing");
        for vin in vins {
            assert_eq!(vin.len(), 17, "{what}: {vin} is not 17 characters");
            assert!(
                vin.bytes().all(|c| !is_ioq(c)),
                "{what}: {vin} carries an I, O or Q"
            );
        }
    }

    #[test]
    fn sweep_is_deterministic_and_well_formed() {
        let Some(db) = embedded() else { return };
        // Not `Exception`: that dimension echoes VinException rows verbatim, which
        // is the one place a corpus VIN is not built to these rules.
        let dims = [Dimension::Wmi, Dimension::Default];
        let a = sweep(db, &dims, YEAR);
        assert_eq!(a, sweep(db, &dims, YEAR));
        assert_well_formed(&a, "sweep");
        // Each dimension is appended whole, so asking for both is asking for each.
        assert_eq!(
            a.len(),
            sweep(db, &[Dimension::Wmi], YEAR).len() + sweep(db, &[Dimension::Default], YEAR).len()
        );
    }

    #[test]
    fn pairwise_is_deterministic_and_well_formed() {
        let Some(db) = embedded() else { return };
        let a = pairwise(db, YEAR, 2000);
        assert_eq!(a, pairwise(db, YEAR, 2000));
        assert_eq!(
            a.len(),
            2000,
            "the limit is exact once the data is this big"
        );
        assert_well_formed(&a, "pairwise");
        // The limit truncates one list rather than selecting a different one.
        assert_eq!(a[..500], pairwise(db, YEAR, 500)[..]);
    }

    #[test]
    fn seeded_is_deterministic_well_formed_and_free_of_duplicates() {
        let Some(db) = embedded() else { return };
        let a = seeded(db, YEAR, 5000);
        assert_eq!(a, seeded(db, YEAR, 5000));
        assert_well_formed(&a, "seeded");
        // The property `generate` deliberately does not have: filler-heavy rows
        // from different schemas collide, and `seeded` drops the repeats.
        let unique: HashSet<&String> = a.iter().collect();
        assert_eq!(unique.len(), a.len(), "seeded repeated a VIN");
        assert_eq!(a[..500], seeded(db, YEAR, 500)[..]);
    }

    #[test]
    fn generate_repeats_itself_and_randomized_fills_rarely_collide() {
        let Some(db) = embedded() else { return };
        let now = crate::now_micros();
        let f = Filter::default();
        let a = generate(db, 500, 7, &f, now, YEAR);
        assert_eq!(a, generate(db, 500, 7, &f, now, YEAR));
        assert_well_formed(&a, "generate");
        // Randomized fills (serial digits, unpinned VDS/plant, free key choices)
        // make collisions vanishingly rare; the contract stays `n` VINs, not `n`
        // distinct ones, so the bound is near-total uniqueness, not exact.
        let unique: HashSet<&String> = a.iter().collect();
        assert_eq!(a.len(), 500);
        assert!(unique.len() > 495, "only {} unique of 500", unique.len());
    }

    #[test]
    fn a_year_range_bounds_the_decoded_model_year_and_samples_inside_it() {
        let Some(db) = embedded() else { return };
        let now = crate::now_micros();
        let f = Filter {
            min_year: Some(2015),
            max_year: Some(2018),
            ..Default::default()
        };
        let vins = generate(db, 100, 3, &f, now, YEAR);
        assert!(!vins.is_empty());
        let mut seen: HashSet<i32> = HashSet::new();
        for vin in &vins {
            let y = crate::decode_full(db, vin, now, YEAR, None)
                .model_year
                .expect("a range-filtered VIN must decode to a model year");
            assert!((2015..=2018).contains(&y), "{vin} decoded to {y}");
            seen.insert(y);
        }
        assert!(seen.len() > 1, "a range should sample the band, not pin it");
        // An empty intersection — inverted range, or a `year` outside it —
        // matches nothing.
        let inverted = Filter {
            min_year: Some(2018),
            max_year: Some(2015),
            ..Default::default()
        };
        assert!(generate(db, 5, 3, &inverted, now, YEAR).is_empty());
        let outside = Filter {
            year: Some(2020),
            min_year: Some(2015),
            max_year: Some(2018),
            ..Default::default()
        };
        assert!(generate(db, 5, 3, &outside, now, YEAR).is_empty());
    }
}
