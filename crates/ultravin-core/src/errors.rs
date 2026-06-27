//! Error-code accumulation and the suggested-VIN / error-bytes machinery.
//!
//! Ports `vpic.spvindecode_errorcode` (codes 2/3/4/5/6/14, corrected VIN, error
//! bytes, unused positions) plus the `spvindecode_core` error assembly that
//! layers on codes 0/1/6/7/8/9/10/11/12/400 and builds AdditionalDecodingInfo
//! (element 156). Every intentional bug is preserved (see comments).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::checkdigit::{check_digit_v1, check_digit_with_flag};
use crate::db::Db;
use crate::decode::CoreResult;
use crate::tables::{Wmi, NULL_I32};
use crate::wmi::vin_wmi;

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

/// Body-style/start-position context used by both the scan and the check digit.
fn start_context(vin: &str, wmi: Option<&Wmi>) -> (usize, bool) {
    let pos3 = vin.as_bytes().get(2).copied();
    if pos3 == Some(b'9') {
        (15, false)
    } else if let Some(w) = wmi {
        let car_lt =
            matches!(w.vehicletypeid, 2 | 7) || (w.vehicletypeid == 3 && w.trucktypeid == 1);
        if car_lt {
            (13, true)
        } else {
            (14, false)
        }
    } else {
        (14, false)
    }
}

fn class1(c: u8) -> bool {
    c.is_ascii_digit() || matches!(c, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z') || c == b'*'
}
fn class_digit(c: u8) -> bool {
    c.is_ascii_digit() || c == b'*'
}
fn class_cd(c: u8) -> bool {
    c.is_ascii_digit() || c == b'X' || c == b'*'
}
fn class_my(c: u8) -> bool {
    matches!(c, b'1'..=b'9')
        || matches!(c, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'T' | b'V'..=b'Y')
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

/// Port of `vpic.fExtractValidCharsPerWmiYear` (== the `WMIYearValidChars`
/// cache, verified byte-equal): VIN-position -> allowed chars, where the VIN
/// position is the key index + 3. Empty when `model_year` is `None`.
fn valid_charset(db: &Db, wmi: &str, model_year: Option<i32>) -> HashMap<i32, BTreeSet<char>> {
    let mut map: HashMap<i32, BTreeSet<char>> = HashMap::new();
    let Some(year) = model_year else {
        return map;
    };
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for wmiid in db.wmi_ids_for_str(wmi) {
        for wvs in db.wmi_vinschema_for(wmiid) {
            let to = if wvs.yearto == NULL_I32 {
                2999
            } else {
                wvs.yearto
            };
            if year < wvs.yearfrom || year > to {
                continue;
            }
            for p in db.patterns_for(wvs.vinschemaid) {
                keys.insert(db.s(p.keys).to_string());
            }
        }
    }
    for key in &keys {
        for (kpos, c) in valid_chars_in_key(key) {
            map.entry(kpos + 3).or_default().insert(c);
        }
    }
    map
}

/// `substring(vin,1,pos-1) || rep || substring(vin, pos+1, 17-pos)`.
fn build_replace(vb: &[char], pos: i32, rep: &str) -> String {
    let p = pos as usize;
    let left: String = vb.iter().take(p.saturating_sub(1)).collect();
    let take = (17 - pos).max(0) as usize;
    let right: String = vb.iter().skip(p).take(take).collect();
    format!("{left}{rep}{right}")
}

/// Output of the `spvindecode_errorcode` helper.
struct ErrorCodeOut {
    codes: Vec<i32>,
    corrected_vin: String,
    error_bytes: String,
    /// `None` mirrors the SQL OUT param left NULL (no unused positions).
    unused_positions: Option<String>,
}

/// Port of `vpic.spvindecode_errorcode` (E0-E6). `matched_keys` are the
/// non-empty `Keys` of the pass's pattern rows (Source ILIKE '%pattern%').
fn errorcode(db: &Db, vin: &str, model_year: Option<i32>, matched_keys: &[String]) -> ErrorCodeOut {
    let var_wmi = vin_wmi(vin);
    let vb: Vec<char> = vin.chars().collect();
    let vlen = vb.len() as i32;
    let mut codes: Vec<i32> = Vec::new();
    let mut corrected_vin = String::new();
    let mut error_bytes = String::new();
    let mut unused_positions: Option<String> = None;

    if var_wmi.chars().count() < 3 {
        codes.push(6);
    }

    // E1/E2: scan positions 4..min(n,len) against the correction charset.
    let charset = valid_charset(db, &var_wmi, model_year);
    let n: i32 = if var_wmi.chars().count() == 6 { 11 } else { 14 };
    let mut corrected = String::new();
    let mut replacements = String::new();
    let mut cnt_errors = 0;
    let mut last_error_pos = 0i32;
    let mut last_replacements = String::new();
    let mut i = 3i32;
    while i < n && i < vlen {
        i += 1;
        let var_c = vb[(i - 1) as usize];
        if i == 9 || i == 10 {
            corrected.push(var_c);
            continue;
        }
        match charset.get(&i) {
            Some(set) if !set.is_empty() => {
                if set.contains(&var_c) {
                    corrected.push(var_c);
                } else {
                    let x: String = set.iter().collect();
                    replacements.push_str(&format!("({i}:{x})"));
                    cnt_errors += 1;
                    last_error_pos = i;
                    last_replacements = x;
                    corrected.push('!');
                }
            }
            _ => corrected.push(var_c), // cntTotal = 0
        }
    }

    // E3: bracket the WMI back on, then tail-fill from the raw VIN.
    let w: Vec<char> = var_wmi.chars().collect();
    let mut corrected = if w.len() == 3 {
        format!("{var_wmi}{corrected}")
    } else {
        let left3: String = w.iter().take(3).collect();
        let right3: String = w.iter().skip(w.len().saturating_sub(3)).collect();
        format!("{left3}{corrected}{right3}")
    };
    let clen = corrected.chars().count();
    if (vlen as usize) > clen {
        let tail: String = vb.iter().skip(clen).take(3).collect();
        corrected.push_str(&tail);
    }

    if cnt_errors == 1 {
        if last_replacements.chars().count() == 1 {
            // E4(a): single candidate -> auto-correct (code 2).
            corrected = build_replace(&vb, last_error_pos, &last_replacements);
            codes.push(2);
            corrected_vin = corrected.clone();
            error_bytes = replacements.clone();
        } else {
            // E4(b): check digit disambiguates among the candidates.
            let mut good = 0;
            let mut new_repl = String::new();
            let mut corrected1 = String::new();
            for var_c in last_replacements.chars() {
                let tmp = build_replace(&vb, last_error_pos, &var_c.to_string());
                let tb: Vec<char> = tmp.chars().collect();
                if tb.len() >= 9 {
                    if let Some(cd) = check_digit_v1(&tmp) {
                        if tb[8] == cd {
                            good += 1;
                            new_repl.push(var_c);
                            corrected1 = tmp.clone();
                        }
                    }
                }
            }
            if good == 1 {
                codes.push(3);
                corrected_vin = corrected1;
                error_bytes = format!("({last_error_pos}:{new_repl})");
            } else {
                codes.push(4);
                corrected_vin = corrected.clone();
                error_bytes = format!("({last_error_pos}:{last_replacements})");
            }
        }
    }
    if cnt_errors > 1 {
        codes.push(5);
        corrected_vin = corrected.clone();
        error_bytes = replacements.clone();
    }

    // E6: unused positions from the matched-pattern keys.
    let mut ty: HashSet<(i32, char)> = HashSet::new();
    for key in matched_keys {
        for (kpos, c) in valid_chars_in_key(key) {
            if c != '|' {
                ty.insert((kpos, c));
            }
        }
    }
    let ubound = 11.min(vlen);
    let mut unused = String::new();
    let mut i = 3i32;
    while i < ubound {
        i += 1;
        if !matches!(i, 4 | 5 | 6 | 7 | 8 | 11) {
            continue;
        }
        let chr = vb[(i - 1) as usize];
        if !ty.contains(&(i - 3, chr)) {
            unused.push(' ');
            unused.push_str(&i.to_string());
        }
    }
    let unused = unused.trim().replace(' ', ",");
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

fn trunc500(s: &str) -> String {
    s.chars().take(500).collect()
}

/// Compute the full error state for a decode pass (the `spvindecode_core` error
/// assembly C1-C11). `var_wmi`/`model_year`/`error12`/`conclusive` are the
/// pass's inputs.
pub fn compute_errors(
    db: &Db,
    vin: &str,
    var_wmi: &str,
    core: &CoreResult,
    model_year: Option<i32>,
    error12: bool,
    conclusive: bool,
) -> ErrorState {
    let items = &core.items;
    let mut raw: BTreeSet<i32> = BTreeSet::new();
    let mut corrected_vin = String::new();
    let mut error_bytes = String::new();
    let mut unused_positions: Option<String> = None;

    // C1: code 7 (no WMI) / code 8 (no pattern) / else the errorcode helper.
    if !core.wmi_found {
        raw.insert(7);
    } else if core.pattern_count == 0 {
        raw.insert(8);
    } else {
        let matched_keys: Vec<String> = items
            .iter()
            .filter(|it| it.source.to_ascii_lowercase().contains("pattern") && !it.keys.is_empty())
            .map(|it| it.keys.clone())
            .collect();
        let ec = errorcode(db, vin, model_year, &matched_keys);
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
        .any(|it| it.element_id == 5 && OFF_ROAD.contains(&it.attribute_id.as_str()));
    if is_off_road {
        raw.insert(10);
    }
    if model_year.is_none() {
        raw.insert(11);
    }

    let vehicle_type: Option<String> = items
        .iter()
        .find(|it| it.element_id == 39)
        .map(|it| it.attribute_id.clone());
    let is_vin_exception = db.vinexception_checkdigit(vin);
    let (start_pos, is_car_mpv_lt) = start_context(vin, db.wmi_any(var_wmi));

    // C5: invalid-char scan; stamps `!` into the corrected VIN AFTER the helper.
    let vb: Vec<char> = vin.chars().collect();
    let vlen = vb.len();
    let mut invalid_chars = String::new();
    let mut cv: Vec<char> = corrected_vin.chars().collect();
    let mut j = 0usize;
    while j < vlen {
        j += 1;
        if j == 9 && (is_off_road || is_vin_exception) {
            continue;
        }
        let c = vb[j - 1] as u8;
        // Mirrors the four-way OR in spvindecode_core verbatim (positions vs
        // char-class). Kept un-factored so it reads against the SQL.
        #[allow(clippy::nonminimal_bool)]
        let bad = (j != 9 && j < start_pos && !class1(c))
            || (j != 9 && j >= start_pos && !class_digit(c))
            || (j == 9 && !class_cd(c))
            || (j == 10 && !class_my(c));
        if bad {
            if cv.is_empty() {
                cv = vb.clone();
            }
            invalid_chars.push_str(&format!(", {}:{}", j, vb[j - 1]));
            // CorrectedVIN = left(cv, j-1) || '!' || substring(cv, j+1, 100).
            let take_left = (j - 1).min(cv.len());
            let mut newcv: Vec<char> = cv[..take_left].to_vec();
            newcv.push('!');
            if j < cv.len() {
                newcv.extend_from_slice(&cv[j..]);
            }
            cv = newcv;
        }
    }
    corrected_vin = cv.iter().collect();

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
        check_digit_valid = vb[8] == calc;
        if !check_digit_valid && !is_vin_exception {
            raw.insert(1);
        }
    }

    // C8: code 0 (clean), then code 14 (clean but no Model element).
    let remaining: BTreeSet<i32> = raw
        .iter()
        .copied()
        .filter(|c| !matches!(c, 9 | 10 | 12))
        .collect();
    if remaining.is_empty() || (remaining.len() == 1 && remaining.contains(&14)) {
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
        info = unused_positions.as_ref().map(|u| {
            trunc500(format!("{} Unused position(s): {}. ", info.unwrap_or_default(), u).trim())
        });
    }
    if raw.contains(&400) {
        let stripped = if invalid_chars.len() > 2 {
            &invalid_chars[2..]
        } else {
            ""
        };
        info = Some(trunc500(
            format!(
                "{} Invalid character(s): {}. ",
                info.unwrap_or_default(),
                stripped
            )
            .trim(),
        ));
    }
    let incomplete = vehicle_type.as_deref() == Some("10")
        || items
            .iter()
            .any(|it| it.element_id == 5 && INCOMPLETE.contains(&it.attribute_id.as_str()));
    if incomplete {
        info = Some(trunc500(
            format!(
                "{} Incomplete Vehicle Warning - Please be advised that the vehicle may have been altered and may not be an accurate representation of the vehicle in its current condition. ",
                info.unwrap_or_default()
            )
            .trim(),
        ));
    }
    if !conclusive {
        info = Some(trunc500(
            format!(
                "{} The Model Year decoded for this VIN may be incorrect. If you know the Model year, please enter it and decode again to get more accurate information. ",
                info.unwrap_or_default()
            )
            .trim(),
        ));
    }

    ErrorState {
        codes: raw.into_iter().collect(),
        corrected_vin,
        error_bytes,
        additional_info: info.unwrap_or_default(),
        is_off_road,
        is_vin_exception,
        check_digit_valid,
    }
}
