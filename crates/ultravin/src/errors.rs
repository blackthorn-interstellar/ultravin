//! Error-code accumulation and the suggested-VIN / error-bytes machinery.
//!
//! Ports `vpic.spvindecode_errorcode` (codes 2/3/4/5/6/14, corrected VIN, error
//! bytes, unused positions) plus the `spvindecode_core` error assembly that
//! layers on codes 0/1/6/7/8/9/10/11/12/400 and builds AdditionalDecodingInfo
//! (element 156). Every intentional bug is preserved (see comments).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::OnceLock;

use crate::checkdigit::{check_digit_v1, check_digit_with_flag, is_default_char, is_my_char};
use crate::db::Db;
use crate::decode::CoreResult;
use crate::hash::FxBuildHasher;
#[cfg(test)]
use crate::tables::ArchivedWmi;
use crate::tables::NULL_I32;

/// The "possible values" payload for element 144, in the reference's order.
///
/// vPIC ships SQL Server, whose database collation is
/// `SQL_Latin1_General_CP1_CI_AS` (verified from `RESTORE HEADERONLY` on
/// VPICList_lite_2026_06.bak). Under it `_` sorts *before* the digits, so the
/// reference emits `(6:_123456789)`. A `BTreeSet` iterates in codepoint order
/// and puts `_` last, which is what ultravin did and is wrong.
///
/// The characters vPIC actually uses here are exactly `_|0-9A-Z`, and over that
/// closed alphabet this key reproduces SQL Server's order exactly. It is not a
/// general implementation of that collation: a non-ASCII letter would sort with
/// the punctuation here and after `Z` in the real thing. `|` never reaches this
/// payload — it only ever occupies VIN position 9, which is skipped before the
/// charset is consulted — so `_` is the only non-alphanumeric that arrives.
#[derive(Debug, Default, Clone)]
pub(crate) struct ValidChars {
    ascii: u128,
    other: std::collections::BTreeSet<char>,
    /// The rendered form, built on first use and kept: a charset is built once
    /// per (WMI, year, position) and memoized, but every VIN with a bad character
    /// at that position renders the same set again — sorting and formatting it
    /// char by char was ~4% of a decode. Cleared on `insert`, so the cache can
    /// never outlive the set it describes.
    rendered: OnceLock<String>,
}

impl ValidChars {
    fn insert(&mut self, c: char) {
        self.rendered.take();
        if c.is_ascii() {
            self.ascii |= 1 << (c as u32);
        } else {
            self.other.insert(c);
        }
    }

    /// The `Display` text, computed once per set.
    fn rendered(&self) -> &str {
        self.rendered.get_or_init(|| {
            let mut chars: Vec<char> = (0..128u8)
                .filter(|c| self.ascii & (1 << c) != 0)
                .map(char::from)
                .chain(self.other.iter().copied())
                .collect();
            chars.sort_by_key(|c| (c.is_ascii_alphanumeric(), *c));
            chars.into_iter().collect()
        })
    }

    fn contains(&self, c: char) -> bool {
        if c.is_ascii() {
            self.ascii & (1 << (c as u32)) != 0
        } else {
            self.other.contains(&c)
        }
    }

    fn is_empty(&self) -> bool {
        self.ascii == 0 && self.other.is_empty()
    }
}

/// Renders in the reference's order — the only way to turn this into a string.
///
/// Deliberately not `Deref`, and the inner set is private: a caller that could
/// reach `.iter()` would get codepoint order, which is the bug this type exists
/// to make unrepresentable.
impl std::fmt::Display for ValidChars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.rendered())
    }
}

/// element-5 attribute ids that flag an off-road PIN (code 10).
const OFF_ROAD: [&str; 10] = [
    "69", "84", "86", "88", "97", "105", "113", "124", "126", "127",
];

/// element-5 attribute ids that flag an incomplete vehicle (156 warning).
const INCOMPLETE: [&str; 16] = [
    "65", "107", "70", "74", "63", "72", "112", "62", "64", "76", "78", "71", "77", "67", "116",
    "75",
];

/// `vpic.errorcode.additionalerrortext` for id 4 (verbatim).
const ADDL_ERR_4: &str = "In the Possible values section, the Numeric value before the : indicates the position in error and the values after the : indicate the possible values that are allowed in this position.";
/// `vpic.errorcode.additionalerrortext` for id 5 (verbatim, no trailing period).
const ADDL_ERR_5: &str = "The error positions are indicated by ! in Suggested VIN. In the Possible values section, each pair of parenthesis indicate information about each error position in VIN . The Numeric value before the : indicates the position in error and the values after the : indicate the possible values that are allowed in this position";

/// Computed error state for one decode pass.
#[derive(Debug, PartialEq, Eq)]
pub struct ErrorState {
    /// Sorted error codes (the element-143 CSV / `error_codes` list).
    pub codes: Vec<i32>,
    /// element 142 (suggested/corrected VIN), after the invalid-char `!` stamp.
    pub corrected_vin: String,
    /// element 144 (error bytes, e.g. `(5:M)`).
    pub error_bytes: String,
    /// element 156 (AdditionalDecodingInfo).
    pub additional_info: String,
    pub is_off_road: bool,
    pub is_vin_exception: bool,
    pub check_digit_valid: bool,
}

/// Body-style/start-position context used by both the scan and check digit.
/// Low-volume VINs use their six-character WMI form regardless of the WMI row's
/// vehicle type, so they deliberately suppress the car/light-truck flag.
fn start_context(vin: &str, is_car_mpv_lt: bool) -> (usize, bool) {
    if vin.as_bytes().get(2) == Some(&b'9') {
        (15, false)
    } else if is_car_mpv_lt {
        (13, true)
    } else {
        (14, false)
    }
}

fn class1(c: u8) -> bool {
    is_default_char(c) || c == b'*'
}
fn class_digit(c: u8) -> bool {
    c.is_ascii_digit() || c == b'*'
}
fn class_cd(c: u8) -> bool {
    c.is_ascii_digit() || c == b'X' || c == b'*'
}
fn class_my(c: u8) -> bool {
    is_my_char(c)
}

/// Port of `vpic.fValidCharsInRegEx`: the set of `validchars` that match a
/// bracket pattern. No `-`/`^` means a literal char list (brackets stripped).
fn valid_chars_in_regex(s: &str) -> String {
    let up = s.to_ascii_uppercase();
    if !up.contains('-') && !up.contains('^') {
        return up.replace([']', '['], "");
    }
    const VALIDCHARS: &str = "ABCDEFGHJKLMNPRSTUVWXYZ0123456789";
    let pattern = format!("^{up}$");
    match regex::Regex::new(&pattern) {
        Ok(re) => VALIDCHARS
            .chars()
            .filter(|c| re.is_match(&c.to_string()))
            .collect(),
        Err(_) => String::new(),
    }
}

/// Port of `vpic.fValidCharsInKey` (strict mode). Returns `(ind, char)` pairs
/// where `ind` is the 1-based index over the key body; `#` expands to 0-9, `*`
/// yields nothing (strict), brackets expand via [`valid_chars_in_regex`].
pub fn valid_chars_in_key(key: &str) -> Vec<(i32, char)> {
    let chars: Vec<char> = key.chars().collect();
    let n = chars.len();
    let mut out: Vec<(i32, char)> = Vec::new();
    let mut inside = false;
    let mut ind: i32 = 0;
    let mut start0 = 0usize;
    let mut i = 0usize;
    while i < n {
        let s = chars[i];
        i += 1; // i is now the 1-based position of s
        if s == '[' && !inside {
            inside = true;
            start0 = i - 1;
            continue;
        }
        if !inside {
            ind += 1;
            match s {
                '#' => {
                    for d in '0'..='9' {
                        out.push((ind, d));
                    }
                }
                '*' => { /* strict mode: nothing */ }
                _ => out.push((ind, s)),
            }
            continue;
        }
        if s == ']' {
            ind += 1;
            let pat: String = chars[start0..i].iter().collect();
            for c in valid_chars_in_regex(&pat).chars() {
                if c != '*' && c != '|' {
                    out.push((ind, c));
                }
            }
            inside = false;
        }
    }
    out
}

/// One key's expanded `(key index, allowed char)` pairs — the [`valid_chars_in_key`]
/// result, shared by `Rc` so a memo hit hands out a pointer, not a copy.
type KeyChars = Rc<[(i32, char)]>;

/// High-water mark for [`KEY_CHARS`]. The archive holds ~74k distinct pattern
/// keys, so a long-running batch that spans every WMI would otherwise grow the
/// memo into double-digit megabytes *per worker thread*. Past the cap it resets;
/// a decode's keys all come from one WMI, so the working set refills at once.
const KEY_CHARS_CAP: usize = 16_384;

thread_local! {
    /// Per-thread memo of [`valid_chars_in_key`] for the E6 unused-position scan,
    /// which expands every matched pattern key on every pass — and, for a bracket
    /// key, compiles a regex to do it (see [`valid_chars_in_regex`]). The
    /// expansion is a pure function of the key text and the keys come from the
    /// immutable archive, so a hit is byte-identical to recomputing; this is the
    /// same trade as the shared valid-charset cache below.
    /// Not shared with [`valid_charset`], which sweeps *every* key of a WMI-year
    /// (already memoized as a whole) and would flood the memo with keys E6 never
    /// asks about. Keys are archive-derived, never caller-derived, so the fast
    /// hasher is safe here for the same reason it is in `hash.rs`.
    static KEY_CHARS: RefCell<HashMap<String, KeyChars, FxBuildHasher>> =
        RefCell::new(HashMap::default());
}

/// [`valid_chars_in_key`], memoized per thread.
fn key_chars(key: &str) -> KeyChars {
    if let Some(hit) = KEY_CHARS.with(|c| c.borrow().get(key).cloned()) {
        return hit;
    }
    let expansion: KeyChars = valid_chars_in_key(key).into();
    KEY_CHARS.with(|c| {
        let mut memo = c.borrow_mut();
        if memo.len() >= KEY_CHARS_CAP {
            memo.clear();
        }
        memo.insert(key.to_string(), Rc::clone(&expansion));
    });
    expansion
}

// The correction helper consults only VIN positions 4..14. Index those
// directly; the public recomputation function still returns every position.
type Charset = [ValidChars; 11];

pub(crate) struct ValidCharsetCache {
    /// Keys are copied only from the immutable archive. Caller input can query
    /// this map but cannot grow it.
    by_wmi: HashMap<String, OnceLock<Box<[CharsetInterval]>>, FxBuildHasher>,
}

struct CharsetInterval {
    start: i64,
    end: i64,
    charset: OnceLock<Option<Box<Charset>>>,
}

impl ValidCharsetCache {
    pub(crate) fn new(db: &Db) -> Self {
        let mut by_wmi = HashMap::default();
        for row in db.wmis() {
            by_wmi
                .entry(db.s(row.wmi.to_native()).to_owned())
                .or_insert_with(OnceLock::new);
        }
        Self { by_wmi }
    }

    fn get<'a>(&'a self, db: &Db, wmi: &str, year: i32) -> Option<&'a Charset> {
        let intervals = self
            .by_wmi
            .get(wmi)?
            .get_or_init(|| charset_intervals(db, wmi));
        let year = i64::from(year);
        let interval = intervals.get(intervals.partition_point(|interval| interval.end <= year))?;
        if year < interval.start {
            return None;
        }
        interval
            .charset
            .get_or_init(|| build_charset(db, wmi, interval.start as i32).map(Box::new))
            .as_deref()
    }
}

/// Partition all i32 years at this WMI's archive range changes. The topology is
/// bounded by archive links; reversed ranges contribute no intervals.
fn charset_intervals(db: &Db, wmi: &str) -> Box<[CharsetInterval]> {
    let mut boundaries = vec![i64::from(i32::MIN), i64::from(i32::MAX) + 1];
    for wmiid in db.wmi_ids_for_str(wmi) {
        for row in db.wmi_vinschema_for(wmiid) {
            let start = i64::from(row.yearfrom.to_native());
            let end = i64::from(row.yearto_or(2999));
            if start <= end {
                boundaries.push(start);
                boundaries.push(end + 1);
            }
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
        .windows(2)
        .map(|bounds| CharsetInterval {
            start: bounds[0],
            end: bounds[1],
            charset: OnceLock::new(),
        })
        .collect()
}

/// The distinct pattern keys covering `wmi` in `year` — the cursor body of
/// `vpic.fExtractValidCharsPerWmiYear`.
fn charset_keys(db: &Db, wmi: &str, year: i32) -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for wmiid in db.wmi_ids_for_str(wmi) {
        for wvs in db.wmi_vinschema_for(wmiid) {
            if year < wvs.yearfrom.to_native() || year > wvs.yearto_or(2999) {
                continue;
            }
            for p in db.patterns_for(wvs.vinschemaid.to_native()) {
                keys.insert(db.s(p.keys.to_native()).to_string());
            }
        }
    }
    keys
}

/// [`valid_charset`] as plain data: VIN position -> allowed chars, ascending.
///
/// The shipped `vpic.WMIYearValidChars` table is a stale snapshot of this same
/// function, so `ultravin-build --stale-cache-report` diffs a dump's cache
/// against this. Not on the decode path; no memo.
pub fn recompute_valid_chars(db: &Db, wmi: &str, year: i32) -> BTreeMap<i32, BTreeSet<char>> {
    let mut map: BTreeMap<i32, BTreeSet<char>> = BTreeMap::new();
    for key in &charset_keys(db, wmi, year) {
        for (kpos, c) in valid_chars_in_key(key) {
            map.entry(kpos + 3).or_default().insert(c);
        }
    }
    map
}

/// Port of `vpic.fExtractValidCharsPerWmiYear`, byte-equal to that function:
/// VIN-position -> allowed chars, where the VIN position is the key index + 3.
/// Empty when `model_year` is `None`. The stored proc prefers the derived
/// `WMIYearValidChars` cache and only calls the function on an empty cell, and
/// the shipped cache is stale for ~2% of (wmi, year) cells — see
/// `docs/KNOWN_DEVIATIONS.md` and the `--stale-cache-report` scan.
fn build_charset(db: &Db, wmi: &str, year: i32) -> Option<Charset> {
    let mut map: Charset = std::array::from_fn(|_| ValidChars::default());
    let mut any = false;
    for key in &charset_keys(db, wmi, year) {
        for (kpos, c) in valid_chars_in_key(key) {
            any = true;
            if let Some(chars) = map.get_mut((kpos - 1) as usize) {
                chars.insert(c);
            }
        }
    }
    any.then_some(map)
}

fn valid_charset<'a>(db: &'a Db, wmi: &str, model_year: Option<i32>) -> Option<&'a Charset> {
    let year = model_year?;
    db.valid_charset_cache().get(db, wmi, year)
}

/// `substring(vin,1,pos-1) || rep || substring(vin, pos+1, 17-pos)`.
fn build_replace(vb: &[char], pos: i32, rep: &str) -> String {
    let p = pos as usize;
    let mut out = String::with_capacity(17 + rep.len());
    out.extend(vb.iter().take(p.saturating_sub(1)));
    out.push_str(rep);
    out.extend(vb.iter().skip(p).take((17 - pos).max(0) as usize));
    out
}

/// VIN characters without a heap allocation for the supported 17-character case.
/// Longer malformed inputs retain the complete character sequence in the fallback.
enum VinChars {
    Stack { chars: [char; 17], len: usize },
    Owned(Vec<char>),
}

impl VinChars {
    fn new(vin: &str) -> Self {
        let mut input = vin.chars();
        let mut chars = ['\0'; 17];
        let mut len = 0;
        while len < chars.len() {
            let Some(value) = input.next() else {
                return Self::Stack { chars, len };
            };
            chars[len] = value;
            len += 1;
        }
        let Some(next) = input.next() else {
            return Self::Stack { chars, len };
        };
        let mut owned = Vec::with_capacity(18 + input.size_hint().0);
        owned.extend_from_slice(&chars);
        owned.push(next);
        owned.extend(input);
        Self::Owned(owned)
    }

    fn as_slice(&self) -> &[char] {
        match self {
            Self::Stack { chars, len } => &chars[..*len],
            Self::Owned(chars) => chars,
        }
    }
}

/// Positions 4 through 14 contribute at most eleven characters.
struct CorrectedChars {
    chars: [char; 11],
    len: usize,
}

impl CorrectedChars {
    fn new() -> Self {
        Self {
            chars: ['\0'; 11],
            len: 0,
        }
    }

    fn push(&mut self, value: char) {
        debug_assert!(self.len < self.chars.len());
        self.chars[self.len] = value;
        self.len += 1;
    }

    fn iter(&self) -> impl Iterator<Item = char> + '_ {
        self.chars[..self.len].iter().copied()
    }
}

/// Output of the `spvindecode_errorcode` helper.
struct ErrorCodeOut {
    codes: Vec<i32>,
    corrected_vin: String,
    error_bytes: String,
    /// `None` mirrors the SQL OUT param left NULL (no unused positions).
    unused_positions: Option<String>,
}

fn correction_position_text(position: i32) -> &'static str {
    match position {
        4 => "4",
        5 => "5",
        6 => "6",
        7 => "7",
        8 => "8",
        9 => "9",
        10 => "10",
        11 => "11",
        12 => "12",
        13 => "13",
        14 => "14",
        _ => unreachable!("correction position is bounded to 4..=14"),
    }
}

fn push_replacement(out: &mut String, position: i32, replacements: &str) {
    let position = correction_position_text(position);
    out.reserve(position.len() + replacements.len() + 3);
    out.push('(');
    out.push_str(position);
    out.push(':');
    out.push_str(replacements);
    out.push(')');
}

/// Port of `vpic.spvindecode_errorcode` (E0-E6). `matched_keys` are the
/// non-empty `Keys` of the pass's pattern rows (Source ILIKE '%pattern%').
fn errorcode<'a>(
    db: &Db,
    vin: &str,
    var_wmi: &str,
    model_year: Option<i32>,
    matched_keys: impl Iterator<Item = &'a str>,
) -> ErrorCodeOut {
    let vb = VinChars::new(vin);
    let vb = vb.as_slice();
    let vlen = vb.len() as i32;
    let mut codes: Vec<i32> = Vec::new();
    let mut corrected_vin = String::new();
    let mut error_bytes = String::new();
    let mut unused_positions: Option<String> = None;

    if var_wmi.chars().count() < 3 {
        codes.push(6);
    }

    // E1/E2: scan positions 4..min(n,len) against the correction charset.
    let charset = valid_charset(db, var_wmi, model_year);
    let n: i32 = if var_wmi.chars().count() == 6 { 11 } else { 14 };
    let mut corrected = CorrectedChars::new();
    let mut replacements = String::new();
    let mut cnt_errors = 0;
    let mut last_error_pos = 0i32;
    let mut last_replacements = "";
    let mut i = 3i32;
    while i < n && i < vlen {
        i += 1;
        let var_c = vb[(i - 1) as usize];
        if i == 9 || i == 10 {
            corrected.push(var_c);
            continue;
        }
        match charset.and_then(|chars| chars.get((i - 4) as usize)) {
            Some(set) if !set.is_empty() => {
                if set.contains(var_c) {
                    corrected.push(var_c);
                } else {
                    let x = set.rendered();
                    push_replacement(&mut replacements, i, x);
                    cnt_errors += 1;
                    last_error_pos = i;
                    last_replacements = x;
                    corrected.push('!');
                }
            }
            _ => corrected.push(var_c), // cntTotal = 0
        }
    }

    // Only ambiguous or multiple errors use this suggested VIN. Clean inputs
    // and single-candidate corrections need no WMI/tail reconstruction.
    let compose_corrected = || {
        let mut out = String::with_capacity(17);
        let wmi_len = var_wmi.chars().count();
        if wmi_len == 3 {
            out.push_str(var_wmi);
            out.extend(corrected.iter());
        } else {
            out.extend(var_wmi.chars().take(3));
            out.extend(corrected.iter());
            out.extend(var_wmi.chars().skip(wmi_len.saturating_sub(3)));
        }
        let len = out.chars().count();
        if (vlen as usize) > len {
            out.extend(vb.iter().skip(len).take(3));
        }
        out
    };

    if cnt_errors == 1 {
        if last_replacements.chars().count() == 1 {
            // E4(a): single candidate -> auto-correct (code 2).
            corrected_vin = build_replace(vb, last_error_pos, last_replacements);
            codes.push(2);
            error_bytes = replacements.clone();
        } else {
            // E4(b): check digit disambiguates among the candidates.
            let mut good = 0;
            let mut new_repl = String::new();
            let mut corrected1 = String::new();
            for var_c in last_replacements.chars() {
                let tmp = build_replace(vb, last_error_pos, &var_c.to_string());
                if let Some(cd) = check_digit_v1(&tmp) {
                    if tmp.chars().nth(8) == Some(cd) {
                        good += 1;
                        new_repl.push(var_c);
                        corrected1 = tmp;
                    }
                }
            }
            if good == 1 {
                codes.push(3);
                corrected_vin = corrected1;
                push_replacement(&mut error_bytes, last_error_pos, &new_repl);
            } else {
                codes.push(4);
                corrected_vin = compose_corrected();
                push_replacement(&mut error_bytes, last_error_pos, last_replacements);
            }
        }
    }
    if cnt_errors > 1 {
        codes.push(5);
        corrected_vin = compose_corrected();
        error_bytes = replacements.clone();
    }

    // E6 only asks whether the VIN's own character is present at six positions.
    // Keep those membership answers, rather than allocating a set containing
    // every possible character from every matched key.
    let used = used_key_positions(vb, matched_keys);
    let ubound = 11.min(vlen);
    let mut unused = String::new();
    let mut i = 3i32;
    while i < ubound {
        i += 1;
        if !matches!(i, 4 | 5 | 6 | 7 | 8 | 11) {
            continue;
        }
        if !used[(i - 4) as usize] {
            // Comma-joined as it is built. The SQL accumulates " N" and then
            // trims + replaces ' ' with ',', which yields exactly this — every
            // part is a bare decimal, so there is no interior space to convert.
            if !unused.is_empty() {
                unused.push(',');
            }
            unused.push_str(correction_position_text(i));
        }
    }
    if !unused.is_empty() {
        codes.push(14);
        unused_positions = Some(unused);
    }

    ErrorCodeOut {
        codes,
        corrected_vin,
        error_bytes,
        unused_positions,
    }
}

/// E6 examines VIN positions 4..8 and 11. Stop visiting pattern rows once
/// all of those positions present in this VIN have a matching character.
fn used_key_positions<'a>(vin: &[char], matched_keys: impl Iterator<Item = &'a str>) -> [bool; 8] {
    let mut used = [false; 8];
    let mut remaining = 0u8;
    for i in [0, 1, 2, 3, 4, 7] {
        if vin.get(i + 3).is_some() {
            remaining |= 1 << i;
        }
    }
    if remaining == 0 {
        return used;
    }
    for key in matched_keys {
        // A key cannot cover more positions than its byte length. In
        // particular, common five-position keys cannot fill VIN position 11.
        if key.len() <= remaining.trailing_zeros() as usize {
            continue;
        }
        if key.is_ascii() && !key.contains('[') {
            let bytes = key.as_bytes();
            let mut todo = remaining;
            while todo != 0 {
                let i = todo.trailing_zeros() as usize;
                todo &= todo - 1;
                if let Some(&byte) = bytes.get(i) {
                    let c = vin[i + 3];
                    if byte != b'*'
                        && byte != b'|'
                        && (if byte == b'#' {
                            c.is_ascii_digit()
                        } else {
                            c == char::from(byte)
                        })
                    {
                        used[i] = true;
                        remaining &= !(1 << i);
                    }
                }
            }
        } else {
            for &(pos, c) in key_chars(key).iter() {
                if matches!(pos, 1..=5 | 8) && c != '|' && vin.get((pos + 2) as usize) == Some(&c) {
                    let index = (pos - 1) as usize;
                    used[index] = true;
                    remaining &= !(1 << index);
                }
            }
        }
        if remaining == 0 {
            break;
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::IntSet;

    #[test]
    fn source_labels_preserve_the_case_insensitive_substring_rule() {
        for source in [
            "Pattern",
            "EngineModelPattern",
            "Formula Pattern",
            "pattern - model",
            "VehType",
            "Manu. Name",
            "Manu. Id",
            "ModelYear",
            "Make",
            "Vehicle Specs",
            "Default",
            "Corrections",
            "Conversion 1: PATTERN",
            "a pAtTeRn suffix",
            "",
            "日本語pattern",
        ] {
            assert_eq!(
                pattern_source(source),
                contains_ci(source, b"pattern"),
                "{source:?}"
            );
        }
    }

    #[test]
    fn compact_valid_chars_preserve_membership_and_render_order() {
        let mut actual = ValidChars::default();
        let mut expected = BTreeSet::new();
        assert!(actual.is_empty());
        for c in "_9Aé|日本\0Z0123\n".chars().chain("é_9".chars()) {
            actual.insert(c);
            expected.insert(c);
            assert!(!actual.is_empty());
            for probe in (0..=127u8).map(char::from).chain("é日本界".chars()) {
                assert_eq!(actual.contains(probe), expected.contains(&probe));
            }
            let mut ordered: Vec<_> = expected.iter().copied().collect();
            ordered.sort_by_key(|c| (c.is_ascii_alphanumeric(), *c));
            assert_eq!(actual.rendered(), ordered.iter().collect::<String>());
        }
    }

    #[test]
    fn stack_errorcode_buffers_preserve_character_sequences() {
        for vin in [
            "",
            "ABC",
            "1HGCM82633A004352",
            "é日本語abcdefghijkl",
            "é日本語abcdefghijklmnop",
            "a malformed input much longer than seventeen characters",
        ] {
            let expected: Vec<_> = vin.chars().collect();
            let actual = VinChars::new(vin);
            assert_eq!(actual.as_slice(), expected);
            assert_eq!(
                matches!(actual, VinChars::Stack { .. }),
                expected.len() <= 17
            );
        }

        for text in ["", "A", "CM8263A0043", "é日本語ABC"] {
            let mut actual = CorrectedChars::new();
            for value in text.chars() {
                actual.push(value);
            }
            assert_eq!(actual.iter().collect::<String>(), text);
        }
    }

    #[test]
    fn position_flags_equal_the_full_character_set() {
        let key_sets: &[&[&str]] = &[
            &[],
            &[""],
            &["CM82[67]", "CM82[67]", "*****|*A"],
            &["[A-Z][1-9]*", "#", "_______________"],
            &["|", "abc", "é"],
            &["#_]*|A#_", "AB#***|#"],
            &["[", "[ABC", "]_#", "***#***#"],
        ];
        for keys in key_sets {
            let expected: IntSet<(i32, char)> = keys
                .iter()
                .flat_map(|key| valid_chars_in_key(key))
                .filter(|(_, c)| *c != '|')
                .collect();
            for vin in [
                "1HGCM82633A004352",
                "ABCZ1Z9|*1X",
                "123abc",
                "123é",
                "1239_]5|A9_",
                "123#*|_]2",
                "",
            ] {
                let vin: Vec<char> = vin.chars().collect();
                let actual = used_key_positions(&vin, keys.iter().copied());
                for i in [0, 1, 2, 3, 4, 7] {
                    let used = actual[i];
                    assert_eq!(
                        used,
                        vin.get(i + 3)
                            .is_some_and(|c| expected.contains(&(i as i32 + 1, *c))),
                        "keys {keys:?}, VIN {vin:?}, position {i}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn trunc500(s: &str) -> String {
    let end = if s.len() <= 500 {
        s.len()
    } else {
        s.char_indices().nth(500).map_or(s.len(), |(i, _)| i)
    };
    s[..end].to_string()
}

fn trim_truncate500(mut value: String) -> String {
    let leading = value.len() - value.trim_start().len();
    if leading != 0 {
        value.drain(..leading);
    }
    let trailing_end = value.trim_end().len();
    value.truncate(trailing_end);
    if value.len() > 500 {
        if let Some((end, _)) = value.char_indices().nth(500) {
            value.truncate(end);
        }
    }
    value
}

fn append_info(info: Option<String>, parts: &[&str]) -> String {
    let mut value = info.unwrap_or_default();
    value.reserve(parts.iter().map(|part| part.len()).sum());
    for part in parts {
        value.push_str(part);
    }
    trim_truncate500(value)
}

/// ASCII case-insensitive substring test without allocating — the proc's
/// `Source ILIKE '%pattern%'` gate. `needle` must already be lowercase ASCII.
fn contains_ci(haystack: &str, needle: &[u8]) -> bool {
    let h = haystack.as_bytes();
    if needle.is_empty() {
        return true;
    }
    if h.len() < needle.len() {
        return false;
    }
    h.windows(needle.len())
        .any(|w| w.iter().zip(needle).all(|(a, b)| a.eq_ignore_ascii_case(b)))
}

/// Fixed source labels need no case-insensitive substring scan. A conversion
/// formula may itself contain "pattern", so dynamic sources retain the scan.
fn pattern_source(source: &str) -> bool {
    match source {
        "Pattern" | "EngineModelPattern" | "Formula Pattern" | "pattern - model" => true,
        "VehType" | "Manu. Name" | "Manu. Id" | "ModelYear" | "Make" | "Vehicle Specs"
        | "Default" | "Corrections" => false,
        _ => contains_ci(source, b"pattern"),
    }
}

/// Codes 0..14 and 400 are the entire error domain produced below.
/// One word preserves uniqueness and numeric order without a tree allocation.
#[derive(Default)]
struct ErrorCodes(u16);

impl ErrorCodes {
    fn insert(&mut self, code: i32) {
        debug_assert!((0..=14).contains(&code) || code == 400);
        self.0 |= 1 << if code == 400 { 15 } else { code };
    }

    fn contains(&self, code: &i32) -> bool {
        self.0 & (1 << if *code == 400 { 15 } else { *code }) != 0
    }

    fn iter(&self) -> impl Iterator<Item = i32> + '_ {
        (0..16)
            .filter(|bit| self.0 & (1 << bit) != 0)
            .map(|bit| if bit == 15 { 400 } else { bit })
    }
}

/// Compute the full error state for a decode pass (the `spvindecode_core` error
/// assembly C1-C11). `var_wmi`/`model_year`/`error12`/`conclusive` are the
/// pass's inputs.
#[cfg(test)]
pub(crate) fn compute_errors(
    db: &Db,
    vin: &str,
    var_wmi: &str,
    core: &CoreResult,
    model_year: Option<i32>,
    error12: bool,
    conclusive: bool,
) -> ErrorState {
    let any_wmi = db.wmi_any(var_wmi);
    compute_errors_with_context(
        db,
        vin,
        var_wmi,
        core,
        model_year,
        error12,
        conclusive,
        any_wmi.map(ArchivedWmi::is_car_mpv_lt).unwrap_or(false),
        db.vinexception_checkdigit(vin),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_errors_with_context(
    db: &Db,
    vin: &str,
    var_wmi: &str,
    core: &CoreResult,
    model_year: Option<i32>,
    error12: bool,
    conclusive: bool,
    is_car_mpv_lt: bool,
    is_vin_exception: bool,
) -> ErrorState {
    let items = &core.items;
    let mut raw = ErrorCodes::default();
    let mut corrected_vin = String::new();
    let mut error_bytes = String::new();
    let mut unused_positions: Option<String> = None;

    // C1: code 7 (no WMI) / code 8 (no PatternId-bearing item) / else the
    // errorcode helper. Per spvindecode_core L380 the code-8 gate counts EVERY
    // DecodingItem with a non-null PatternId (regular/engine/formula patterns,
    // pattern-model make, propagated conversions, vehicle specs) — not just the
    // regular Pattern matches.
    if !core.wmi_found {
        raw.insert(7);
    } else if !items.iter().any(|it| it.pattern_id != NULL_I32) {
        raw.insert(8);
    } else {
        let matched_keys = items
            .iter()
            .filter(|it| !it.keys.is_empty() && pattern_source(it.source.as_ref()))
            .map(|it| it.keys.as_ref());
        let ec = errorcode(db, vin, var_wmi, model_year, matched_keys);
        for c in ec.codes {
            raw.insert(c);
        }
        corrected_vin = ec.corrected_vin;
        error_bytes = ec.error_bytes;
        unused_positions = ec.unused_positions;
    }

    // C2: glider (9), off-road (10), missing model year (11).
    let is_engine_off_road = items
        .iter()
        .any(|it| it.element_id == 5 && it.attribute_id == "64");
    if is_engine_off_road {
        raw.insert(9);
    }
    let is_off_road = items
        .iter()
        .any(|it| it.element_id == 5 && OFF_ROAD.contains(&it.attribute_id.as_ref()));
    if is_off_road {
        raw.insert(10);
    }
    if model_year.is_none() {
        raw.insert(11);
    }

    let vehicle_type: Option<&str> = items
        .iter()
        .find(|it| it.element_id == 39)
        .map(|it| it.attribute_id.as_ref());
    let (start_pos, is_car_mpv_lt) = start_context(vin, is_car_mpv_lt);

    // C5: invalid-char scan; stamps `!` into the corrected VIN AFTER the helper.
    let vb = vin.as_bytes();
    let vlen = vb.len();
    let mut invalid_chars = String::new();
    let mut stamped: Option<Vec<char>> = None;
    let mut j = 0usize;
    while j < vlen {
        j += 1;
        if j == 9 && (is_off_road || is_vin_exception) {
            continue;
        }
        let c = vb[j - 1];
        // Mirrors the four-way OR in spvindecode_core verbatim (positions vs
        // char-class). Kept un-factored so it reads against the SQL.
        #[allow(clippy::nonminimal_bool)]
        let bad = (j != 9 && j < start_pos && !class1(c))
            || (j != 9 && j >= start_pos && !class_digit(c))
            || (j == 9 && !class_cd(c))
            || (j == 10 && !class_my(c));
        if bad {
            let cv = stamped.get_or_insert_with(|| {
                if corrected_vin.is_empty() {
                    vin.chars().collect()
                } else {
                    corrected_vin.chars().collect()
                }
            });
            let _ = std::fmt::Write::write_fmt(
                &mut invalid_chars,
                format_args!(", {}:{}", j, c as char),
            );
            // CorrectedVIN = left(cv, j-1) || '!' || substring(cv, j+1, 100).
            // For a monotonically increasing `j` that prefix+'!'+suffix rebuild is
            // exactly an in-place stamp of `!` at index j-1 — O(1) here instead of
            // rebuilding the whole Vec per bad char (which was O(vlen^2) on a long
            // all-invalid input). When j-1 is at or past the current end (a
            // corrected VIN shorter than the input), `left(cv, j-1)` caps at the
            // length, so the `!` lands at the end rather than at j-1 — an append.
            let idx = j - 1;
            if idx < cv.len() {
                cv[idx] = '!';
            } else {
                cv.push('!');
            }
        }
    }
    if let Some(cv) = stamped {
        corrected_vin = cv.into_iter().collect();
    }

    // C6: invalid chars (400), caller-year mismatch (12).
    if !invalid_chars.is_empty() {
        raw.insert(400);
    }
    if error12 {
        raw.insert(12);
    }

    // C7: incomplete VIN (6) / check digit (1). DefaultValues already inserted.
    let mut check_digit_valid = false;
    if vlen < 17 {
        raw.insert(6);
    } else if let Some(calc) = check_digit_with_flag(vin, is_car_mpv_lt) {
        // Bug-for-bug parity, NOT a bug (do not "fix"): `check_digit_with_flag`
        // (fVINCheckDigit2) returns '?' on any invalid char, so a VIN with a literal
        // '?' at position 9 compares '?' == '?' here and reads as *valid*. The oracle
        // does the same — spvindecode compares `cd <> calcCD`, '?' <> '?' is false, so
        // it emits no code 1. Rejecting the '?' sentinel would diverge from the oracle
        // (the spec) and force an answer-key rebuild. Frozen in the parity corpus.
        check_digit_valid = vb[8] as char == calc;
        if !check_digit_valid && !is_vin_exception {
            raw.insert(1);
        }
    }

    // C8: code 0 (clean), then code 14 (clean but no Model element). `raw` is a
    // unique set, so one scan replaces the throwaway `remaining` BTreeSet: clean
    // means no codes outside {9,10,12}, or exactly {14} among them.
    let mut non_special = 0usize;
    let mut has_14 = false;
    for c in raw.iter() {
        if matches!(c, 9 | 10 | 12) {
            continue;
        }
        non_special += 1;
        has_14 |= c == 14;
    }
    if non_special == 0 || (non_special == 1 && has_14) {
        raw.insert(0);
    }
    let has_model = items.iter().any(|it| it.element_id == 28);
    if raw.contains(&0) && !has_model {
        raw.insert(14);
    }

    // C9: AdditionalDecodingInfo (156). `info = None` mirrors a SQL NULL.
    let mut info: Option<String> = None;
    if raw.contains(&4) {
        info = Some(ADDL_ERR_4.to_string());
    }
    if raw.contains(&5) {
        info = Some(ADDL_ERR_5.to_string());
    }
    if raw.contains(&14) {
        // `prev || ' Unused position(s): ' || UnUsedPositions || '. '`; a NULL
        // UnUsedPositions makes the whole concat NULL (no-model code-14 case).
        info = unused_positions
            .as_ref()
            .map(|u| append_info(info, &[" Unused position(s): ", u, ". "]));
    }
    if raw.contains(&400) {
        let stripped = if invalid_chars.len() > 2 {
            &invalid_chars[2..]
        } else {
            ""
        };
        info = Some(append_info(
            info,
            &[" Invalid character(s): ", stripped, ". "],
        ));
    }
    let incomplete = vehicle_type == Some("10")
        || items
            .iter()
            .any(|it| it.element_id == 5 && INCOMPLETE.contains(&it.attribute_id.as_ref()));
    if incomplete {
        info = Some(append_info(
            info,
            &[" Incomplete Vehicle Warning - Please be advised that the vehicle may have been altered and may not be an accurate representation of the vehicle in its current condition. "],
        ));
    }
    if !conclusive {
        info = Some(append_info(
            info,
            &[" The Model Year decoded for this VIN may be incorrect. If you know the Model year, please enter it and decode again to get more accurate information. "],
        ));
    }

    ErrorState {
        codes: raw.iter().collect(),
        corrected_vin,
        error_bytes,
        additional_info: info.unwrap_or_default(),
        is_off_road,
        is_vin_exception,
        check_digit_valid,
    }
}

#[cfg(test)]
mod malformed_class_tests {
    use super::*;

    #[test]
    fn low_volume_wmi_suppresses_car_flag_for_check_digit_context() {
        assert_eq!(start_context("1F9TC25FTAB123456", true), (15, false));
        assert_eq!(start_context("1HGCM82633A004352", true), (13, true));
        assert_eq!(start_context("ZZZCM82633A004352", false), (14, false));
    }

    #[test]
    fn compact_codes_keep_numeric_order_and_remove_duplicates() {
        let mut actual = ErrorCodes::default();
        let mut expected = BTreeSet::new();
        for code in [400, 14, 0, 4, 5, 2, 400, 1, 12, 3, 11, 10, 9, 8, 7, 6] {
            actual.insert(code);
            expected.insert(code);
            assert_eq!(
                actual.iter().collect::<Vec<_>>(),
                expected.iter().copied().collect::<Vec<_>>()
            );
            for probe in (0..=14).chain([400]) {
                assert_eq!(actual.contains(&probe), expected.contains(&probe));
            }
        }
    }

    #[test]
    fn replacement_keeps_character_slices_and_the_seventeen_position_limit() {
        for vin in [
            "",
            "ABC",
            "1HGCM82633A004352",
            "é日本語abcdefghijklmnop",
            "a very long input string",
        ] {
            let chars: Vec<_> = vin.chars().collect();
            for pos in 1..=20 {
                for replacement in ["", "X", "é日本", "ABC"] {
                    let left: String = chars
                        .iter()
                        .take((pos as usize).saturating_sub(1))
                        .collect();
                    let right: String = chars
                        .iter()
                        .skip(pos as usize)
                        .take((17i32 - pos).max(0) as usize)
                        .collect();
                    assert_eq!(
                        build_replace(&chars, pos, replacement),
                        format!("{left}{replacement}{right}")
                    );
                }
            }
        }
    }

    #[test]
    fn truncation_preserves_character_boundaries() {
        for pattern in ["a", "é", "😀", "aé😀中"] {
            for len in [0, 1, 124, 125, 126, 249, 250, 251, 499, 500, 501, 999, 1000] {
                let value: String = pattern.chars().cycle().take(len).collect();
                let expected: String = value.chars().take(500).collect();
                assert_eq!(trunc500(&value), expected, "{pattern:?}, {len}");
            }
        }
    }

    #[test]
    fn static_replacement_positions_match_integer_formatting() {
        for position in 4..=14 {
            for replacements in ["", "A", "é日本", "ABCDEFGH"] {
                let mut actual = String::new();
                push_replacement(&mut actual, position, replacements);
                assert_eq!(actual, format!("({position}:{replacements})"));
            }
        }
    }

    #[test]
    fn in_place_info_append_matches_trimmed_truncation() {
        let values = [
            String::new(),
            "  existing  ".to_string(),
            "\u{2003}Unicode 日本\u{2003}".to_string(),
            "é".repeat(499),
            "😀".repeat(500),
            "中".repeat(501),
        ];
        let suffixes = [
            " Unused position(s): 4,11. ",
            " Invalid character(s): 2:I. ",
            " \u{2003}Unicode suffix\u{2003} ",
            " x",
        ];
        for value in &values {
            for suffix in suffixes {
                let expected = trunc500(format!("{value}{suffix}").trim());
                assert_eq!(
                    append_info(Some(value.clone()), &[suffix]),
                    expected,
                    "value chars={}, suffix={suffix:?}",
                    value.chars().count()
                );
            }
        }

        let mut actual = None;
        let mut expected = None;
        for suffix in suffixes.into_iter().cycle().take(12) {
            actual = Some(append_info(actual, &[suffix]));
            expected = Some(trunc500(
                format!("{}{suffix}", expected.unwrap_or_default()).trim(),
            ));
            assert_eq!(actual, expected);
        }
    }

    /// docs/KNOWN_DEVIATIONS.md #1. `pattern` rows 1827685/1827686 (vinschema
    /// 24522, WMI 7T0, MY 2023-2025) carry the key `*****|*[1-A-JT]`. Postgres
    /// refuses to compile that class ("invalid character range") and aborts the
    /// whole `spvindecode`, so the oracle has no answer for those VINs. We
    /// tolerate it: `1-A` is an ascending range and the second `-` is a literal,
    /// which is also how the SQL Server engine vPIC is authored on reads it.
    #[test]
    fn the_7t0_malformed_class_expands_instead_of_aborting() {
        assert_eq!(valid_chars_in_regex("[1-A-JT]"), "AJT123456789");
    }

    /// The whole key, as `fValidCharsInKey` walks it: `*` yields nothing in
    /// strict mode, the literal `|` keeps its index, and the bracket group lands
    /// on index 8. A crash here would mean we had adopted the oracle's defect.
    #[test]
    fn the_7t0_key_expands_at_the_bracket_index() {
        let got = valid_chars_in_key("*****|*[1-A-JT]");
        assert_eq!(got.first(), Some(&(6, '|')));
        let bracket: String = got
            .iter()
            .filter(|(i, _)| *i == 8)
            .map(|(_, c)| *c)
            .collect();
        assert_eq!(bracket, "AJT123456789");
    }

    /// A well-formed class from the same WMI's other schema (28060) is unaffected
    /// — this deviation is one malformed datum, not a change of rule.
    #[test]
    fn a_well_formed_class_is_unchanged() {
        let got = valid_chars_in_key("*[ZAGR1]");
        let chars: String = got.iter().map(|(_, c)| *c).collect();
        assert_eq!(chars, "ZAGR1");
    }
}

#[cfg(test)]
mod collation_tests {
    use super::*;

    #[test]
    fn underscore_sorts_before_the_digits() {
        // SQL_Latin1_General_CP1_CI_AS, which is what vPIC ships, orders the
        // payload `_0129AZ`. Codepoint order would put `_` last.
        let mut set = ValidChars::default();
        "_0129AZ".chars().for_each(|c| set.insert(c));
        assert_eq!(set.to_string(), "_0129AZ");
    }

    #[test]
    fn an_alphanumeric_only_set_is_unchanged() {
        let mut set = ValidChars::default();
        "9A0Z".chars().for_each(|c| set.insert(c));
        assert_eq!(set.to_string(), "09AZ");
    }

    #[test]
    fn the_whole_vpic_alphabet_matches_the_reference() {
        // Every distinct character in vpic.WMIYearValidChars for 2026_07, in the
        // order SQL Server returns for it.
        let mut set = ValidChars::default();
        "_|0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
            .chars()
            .for_each(|c| set.insert(c));
        assert_eq!(set.to_string(), "_|0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ");
    }
}

#[cfg(test)]
mod shared_charset_tests {
    use super::*;
    use crate::tables::{serialize_artifact, Pattern, VpicData, Wmi, WmiVinSchema};

    fn test_db(first_key: &str, second_key: &str) -> Db {
        let strings = [
            "ABC",
            first_key,
            second_key,
            "E**********",
            "F**********",
            "Z**********",
        ];
        let mut arena_bytes = Vec::new();
        let mut arena_offsets = vec![0];
        for value in strings {
            arena_bytes.extend_from_slice(value.as_bytes());
            arena_offsets.push(arena_bytes.len() as u32);
        }
        let pattern = |id, schema, keys| Pattern {
            id,
            vinschemaid: schema,
            keys,
            keys_regex: 0,
            elementid: 1,
            attributeid: 0,
            createdon_key: 0,
            specificity: 0,
            has_bracket: false,
        };
        let wmi = |id| Wmi {
            id,
            wmi: 0,
            manufacturerid: 1,
            makeid: 1,
            vehicletypeid: 2,
            trucktypeid: 0,
            publicavailabilitydate: 0,
            createdon_key: 0,
        };
        let data = VpicData {
            arena_bytes,
            arena_offsets,
            wmi: vec![wmi(1), wmi(2)],
            wmi_vinschema: vec![
                WmiVinSchema {
                    id: 1,
                    wmiid: 1,
                    vinschemaid: 10,
                    yearfrom: 2000,
                    yearto: 2005,
                },
                WmiVinSchema {
                    id: 2,
                    wmiid: 2,
                    vinschemaid: 20,
                    yearfrom: 2000,
                    yearto: 2005,
                },
                WmiVinSchema {
                    id: 3,
                    wmiid: 2,
                    vinschemaid: 30,
                    yearfrom: 2006,
                    yearto: 2010,
                },
                WmiVinSchema {
                    id: 4,
                    wmiid: 2,
                    vinschemaid: 40,
                    yearfrom: 2998,
                    yearto: NULL_I32,
                },
                WmiVinSchema {
                    id: 5,
                    wmiid: 2,
                    vinschemaid: 50,
                    yearfrom: i32::MAX,
                    yearto: i32::MAX,
                },
                WmiVinSchema {
                    id: 6,
                    wmiid: 2,
                    vinschemaid: 60,
                    yearfrom: 2020,
                    yearto: 2019,
                },
            ],
            vinschema: vec![],
            pattern: vec![
                pattern(1, 10, 1),
                pattern(2, 20, 2),
                pattern(3, 30, 2),
                pattern(4, 40, 4),
                pattern(5, 50, 3),
                pattern(6, 60, 5),
            ],
            element: vec![],
            make_model: vec![],
            wmi_make: vec![],
            enginemodel: vec![],
            enginemodelpattern: vec![],
            defaultvalue: vec![],
            vinexception: vec![],
            conversion: vec![],
            lookups: vec![],
            cover: vec![],
            vspecschema: vec![],
            vspecschemapattern: vec![],
            vspecpattern: vec![],
            vspecschemamodel: vec![],
            vspecschemayear: vec![],
        };
        Db::from_bytes(&serialize_artifact(&data, 1)).expect("test database")
    }

    fn allows(db: &Db, year: i32, value: char) -> bool {
        valid_charset(db, "ABC", Some(year)).is_some_and(|charset| charset[0].contains(value))
    }

    #[test]
    fn cache_is_scoped_to_its_database() {
        let first = test_db("A**********", "B**********");
        let second = test_db("C**********", "D**********");
        assert!(allows(&first, 2000, 'A'));
        assert!(!allows(&first, 2000, 'C'));
        assert!(allows(&second, 2000, 'C'));
        assert!(!allows(&second, 2000, 'A'));
    }

    #[test]
    fn duplicate_wmis_share_inclusive_interval_boundaries() {
        let db = test_db("A**********", "B**********");
        assert!(!allows(&db, 1999, 'A'));
        assert!(allows(&db, 2000, 'A'));
        assert!(allows(&db, 2000, 'B'), "duplicate WMI ranges are unioned");
        assert!(allows(&db, 2005, 'A'));
        assert!(allows(&db, 2005, 'B'));
        assert!(allows(&db, 2006, 'B'));
        assert!(allows(&db, 2010, 'B'));
        assert!(valid_charset(&db, "ABC", Some(2011)).is_none());
        assert!(valid_charset(&db, "ABC", Some(2020)).is_none());
        assert!(allows(&db, 2998, 'F'));
        assert!(allows(&db, 2999, 'F'));
        assert!(valid_charset(&db, "ABC", Some(3000)).is_none());
        assert!(allows(&db, i32::MAX, 'E'));
        assert!(valid_charset(&db, "UNKNOWN", Some(2000)).is_none());

        let first = valid_charset(&db, "ABC", Some(2000)).expect("first interval");
        let last = valid_charset(&db, "ABC", Some(2005)).expect("same interval");
        assert!(std::ptr::eq(first, last));

        for year in [
            1999,
            2000,
            2005,
            2006,
            2010,
            2011,
            2998,
            2999,
            3000,
            i32::MAX,
        ] {
            let oracle = recompute_valid_chars(&db, "ABC", year);
            for value in "ABEFZ".chars() {
                let cached = valid_charset(&db, "ABC", Some(year))
                    .is_some_and(|charset| charset[0].contains(value));
                let recomputed = oracle.get(&4).is_some_and(|chars| chars.contains(&value));
                assert_eq!(cached, recomputed, "year={year}, value={value}");
            }
        }
    }
}

#[cfg(test)]
mod c5_stamp_tests {
    /// The original C5 corrected-VIN rebuild, kept verbatim as the oracle the
    /// shipped in-place stamp must reproduce byte-for-byte: for each bad position
    /// `j` (1-indexed, ascending) `cv = left(cv, j-1) || '!' || substring(cv, j+1)`.
    /// This is the O(vlen) rebuild-per-char that made a long invalid input O(vlen^2).
    fn stamp_rebuild(mut cv: Vec<char>, bad: &[usize]) -> Vec<char> {
        for &j in bad {
            let take_left = (j - 1).min(cv.len());
            let mut newcv: Vec<char> = cv[..take_left].to_vec();
            newcv.push('!');
            if j < cv.len() {
                newcv.extend_from_slice(&cv[j..]);
            }
            cv = newcv;
        }
        cv
    }

    /// The O(1)-per-stamp form now shipped in `compute_errors`.
    fn stamp_inplace(mut cv: Vec<char>, bad: &[usize]) -> Vec<char> {
        for &j in bad {
            let idx = j - 1;
            if idx < cv.len() {
                cv[idx] = '!';
            } else {
                cv.push('!');
            }
        }
        cv
    }

    #[test]
    fn inplace_stamp_matches_the_rebuild() {
        // Covers in-range stamps, a stamp exactly at the last index, and the
        // grow-past-end edge where the corrected VIN is shorter than the bad
        // positions (the `!` must land at the end, not at j-1).
        let cases: &[(&str, &[usize])] = &[
            ("ABCDE", &[1]),
            ("ABCDE", &[3]),
            ("ABCDE", &[5]),
            ("ABCDE", &[1, 3, 5]),
            ("ABC", &[3]),    // stamp exactly at the end index -> replace last
            ("ABC", &[4]),    // one past the end -> append
            ("ABC", &[5]),    // two past the end -> still just appends at the end
            ("ABC", &[2, 5]), // in-range then grow-past-end
            ("ABC", &[4, 5, 6]),
            ("ABC", &[2, 4, 6, 8]),
            ("", &[1]), // degenerate empty start (transform still agrees)
        ];
        for (s, bad) in cases {
            let cv: Vec<char> = s.chars().collect();
            assert_eq!(
                stamp_inplace(cv.clone(), bad),
                stamp_rebuild(cv, bad),
                "mismatch for cv={s:?} bad={bad:?}"
            );
        }
    }
}
