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
//!   to a WMI, make, model year or vehicle type.
//! - [`Db::cover`] — the smallest set that exercises every decode behaviour the
//!   data can reach (computed when the artifact is built).
//! - [`sweep`] — one VIN per row of every dimension that can change a decode,
//!   plus the cases that are not rows at all. Hundreds of thousands of VINs.

use std::collections::BTreeSet;

use crate::db::Db;

/// Model-year characters for 2010..=2039 (I/O/Q and U/Z excluded, 30-year cycle).
const MY_CHARS: &[u8; 30] = b"ABCDEFGHJKLMNPRSTVWXY123456789";
/// VDS/plant fill; the serial gets digits so the check digit lands on a real VIN.
const FILL: u8 = b'A';
const FILL_SERIAL: u8 = b'1';

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
fn key_positions(keys: &str) -> Vec<Option<u8>> {
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
                out.push(Some(b'1'));
                i += 1;
            }
            b'[' => match crate::keyspec::class_body(b, i) {
                Some((body, next)) => {
                    out.push(first_class_member(body));
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

/// The character a bracket class contributes: the lowest member that is legal in
/// a VIN (I/O/Q excluded). If every member is I/O/Q, returns that illegal member
/// so `build_vin` drops the candidate rather than silently mismatching the
/// pattern; `None` only when the class has no members at all (e.g. a reversed
/// range), which leaves the position at its fill.
fn first_class_member(body: &[u8]) -> Option<u8> {
    let mut legal: Option<u8> = None;
    let mut any: Option<u8> = None;
    for (lo, hi) in crate::keyspec::class_ranges(body) {
        if lo > hi {
            continue; // a reversed range contributes nothing
        }
        any = Some(any.map_or(lo, |m| m.min(lo)));
        if let Some(c) = (lo..=hi).find(|&c| !is_ioq(c)) {
            legal = Some(legal.map_or(c, |m| m.min(c)));
        }
    }
    legal.or(any)
}

/// Build a 17-character VIN for `wmi` that satisfies `keys` at model year `year`,
/// with a correct check digit, or `None` when the WMI or a key would force an
/// I/O/Q character into it. Those are illegal in a VIN and have no check-digit
/// transliteration, so such a candidate is skipped rather than emitted malformed.
///
/// A 6-character (low-volume) WMI also fills positions 12-14, which is where
/// `fVinWMI` looks for the rest of it when position 3 is `9`.
pub fn build_vin(wmi: &str, keys: &str, year: i32) -> Option<String> {
    let mut vin = [FILL; 17];
    vin[11..17].fill(FILL_SERIAL);

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
        for (i, ch) in key_positions(vds).into_iter().take(5).enumerate() {
            if let Some(c) = ch {
                vin[3 + i] = c;
            }
        }
    }
    if let Some(vis) = parts.next() {
        for (i, ch) in key_positions(vis).into_iter().take(8).enumerate() {
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
    Some(String::from_utf8_lossy(&vin).into_owned())
}

/// Stamp position 9 (index 8) with the computed check digit. The `'0'` fallback
/// still guards any shape `check_digit` rejects (a letter in a numeric-only
/// position); with I/O/Q gone it is not reached in practice, but it keeps the VIN
/// well formed rather than panicking.
pub(crate) fn stamp_check_digit(vin: &mut [u8]) {
    vin[8] = b'0';
    let text = String::from_utf8_lossy(vin).into_owned();
    vin[8] = match crate::check_digit(&text) {
        Some(c) if c != '?' => c as u8,
        _ => b'0',
    };
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

/// `n` valid VINs matching `filter`, deterministic for a given `seed`.
///
/// Each VIN is drawn from a real WMI, a schema that WMI actually uses, and one of
/// that schema's patterns, so it decodes to real vehicle attributes rather than
/// to an unknown-manufacturer error. Returns fewer than `n` only when the filter
/// matches nothing.
pub fn generate(db: &Db, n: usize, seed: u64, filter: &Filter, current_year: i32) -> Vec<String> {
    let makeids = filter
        .make
        .as_deref()
        .map(|m| db.lookup_ids_by_name(crate::tables::element_lookup_tag(26).unwrap_or(0), m));

    let candidates: Vec<&crate::tables::ArchivedWmi> = db
        .wmis()
        .iter()
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

    let mut rng = Rng(seed);
    // `n` is caller-supplied; a huge value would abort in the allocator before the
    // attempt bound can limit the work. The Vec grows as needed, so only cap the
    // starting capacity. The Python boundary rejects absurd `n` outright.
    let mut out = Vec::with_capacity(n.min(65_536));
    // Bounded: a filtered WMI set can contain WMIs with no usable schema, and we
    // must not spin forever looking for one.
    let mut attempts = 0usize;
    while out.len() < n && attempts < n.saturating_mul(64).max(4096) {
        attempts += 1;
        let w = candidates[rng.below(candidates.len())];
        let links: Vec<_> = db
            .wmi_vinschema_for(w.id.to_native())
            .iter()
            .filter(|l| match filter.year {
                Some(y) => y >= l.yearfrom.to_native() && y <= year_to(l),
                None => true,
            })
            .collect();
        if links.is_empty() {
            continue;
        }
        let link = links[rng.below(links.len())];
        let year = filter
            .year
            .unwrap_or_else(|| pick_year(link.yearfrom.to_native(), year_to(link), current_year));

        let patterns = db.patterns_for(link.vinschemaid.to_native());
        let keys = if patterns.is_empty() {
            "*****".to_string()
        } else {
            db.s(patterns[rng.below(patterns.len())].keys.to_native())
                .to_string()
        };
        out.extend(build_vin(db.s(w.wmi.to_native()), &keys, year));
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
            Dimension::Pattern => sweep_patterns(db, current_year, &mut out),
            Dimension::Engine => sweep_engines(db, current_year, &mut out),
            Dimension::VehicleSpec => sweep_vspecs(db, current_year, &mut out),
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

/// Index from schema id to a WMI that uses it, so a pattern can be turned into a
/// VIN. Built once per sweep; `wmi_vinschema` is keyed the other way round.
pub(crate) fn schema_to_wmi(db: &Db) -> Vec<(i32, i32, i32, i32)> {
    let mut pairs: Vec<(i32, i32, i32, i32)> = Vec::new();
    for w in db.wmis() {
        for l in db.wmi_vinschema_for(w.id.to_native()) {
            pairs.push((
                l.vinschemaid.to_native(),
                w.id.to_native(),
                l.yearfrom.to_native(),
                year_to(l),
            ));
        }
    }
    pairs.sort_unstable();
    pairs.dedup_by_key(|p| p.0);
    pairs
}

/// WMI strings by row id. `wmi` is sorted by the WMI *string*, so a lookup by id
/// is a scan; doing that per pattern is the difference between seconds and hours.
pub(crate) fn wmis_by_id(db: &Db) -> Vec<(i32, &str)> {
    let mut index: Vec<(i32, &str)> = db
        .wmis()
        .iter()
        .map(|w| (w.id.to_native(), db.s(w.wmi.to_native())))
        .collect();
    index.sort_unstable_by_key(|e| e.0);
    index
}

pub(crate) fn wmi_string<'a>(index: &[(i32, &'a str)], wmiid: i32) -> Option<&'a str> {
    index
        .binary_search_by_key(&wmiid, |e| e.0)
        .ok()
        .map(|i| index[i].1)
}

fn sweep_patterns(db: &Db, current_year: i32, out: &mut Vec<String>) {
    let index = schema_to_wmi(db);
    let wmis = wmis_by_id(db);
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
        let entry = match index.binary_search_by_key(&sid, |e| e.0) {
            Ok(i) => index[i],
            Err(_) => continue,
        };
        if let Some(wmi) = wmi_string(&wmis, entry.1) {
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

fn sweep_engines(db: &Db, current_year: i32, out: &mut Vec<String>) {
    let index = schema_to_wmi(db);
    let wmis = wmis_by_id(db);
    let by_name = patterns_by_attribute(db, 18, true);
    for em in db.enginemodels() {
        let name = db.s(em.name.to_native()).trim().to_ascii_lowercase();
        let hit = by_name
            .binary_search_by(|probe| probe.0.as_str().cmp(name.as_str()))
            .ok()
            .map(|i| &db.patterns()[by_name[i].1]);
        let Some(p) = hit else { continue };
        let sid = p.vinschemaid.to_native();
        let Ok(i) = index.binary_search_by_key(&sid, |e| e.0) else {
            continue;
        };
        let entry = index[i];
        if let Some(wmi) = wmi_string(&wmis, entry.1) {
            out.extend(build_vin(
                wmi,
                db.s(p.keys.to_native()),
                pick_year(entry.2, entry.3, current_year),
            ));
        }
    }
}

fn sweep_vspecs(db: &Db, current_year: i32, out: &mut Vec<String>) {
    let index = schema_to_wmi(db);
    let wmis = wmis_by_id(db);
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
        let sid = p.vinschemaid.to_native();
        let Ok(i) = index.binary_search_by_key(&sid, |e| e.0) else {
            continue;
        };
        let entry = index[i];
        let year = years
            .iter()
            .map(|y| y.year.to_native())
            .find(|y| *y >= entry.2 && *y <= entry.3)
            .unwrap_or_else(|| pick_year(entry.2, entry.3, current_year));
        if let Some(wmi) = wmi_string(&wmis, entry.1) {
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
    let index = schema_to_wmi(db);
    let wmis = wmis_by_id(db);
    let mut out = Vec::new();
    for &(schema, wmiid, yearfrom, yearto) in &index {
        if limit > 0 && out.len() >= limit {
            break;
        }
        let Some(wmi) = wmi_string(&wmis, wmiid) else {
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
    let index = schema_to_wmi(db);
    let wmis = wmis_by_id(db);
    let mut out = Vec::new();

    for &(schema, wmiid, yearfrom, yearto) in &index {
        if limit > 0 && out.len() >= limit {
            break;
        }
        let Some(wmi) = wmi_string(&wmis, wmiid) else {
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
            out.extend(emit(wmi, &classes, &filled, year));
        }
        // Whatever the rules did not incidentally cover.
        while !uncovered.is_empty() {
            let &(p1, c1, p2, c2) = uncovered.iter().next().expect("non-empty");
            let mut row: Vec<Option<usize>> = vec![None; levels.len()];
            row[p1] = Some(c1);
            row[p2] = Some(c2);
            let filled = fill_and_retire(&mut row, &levels, &mut uncovered);
            out.extend(emit(wmi, &classes, &filled, year));
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
