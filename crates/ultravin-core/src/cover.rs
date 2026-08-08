//! The smallest VIN set that exercises every decode behaviour the data can reach.
//!
//! One VIN is evidence for dozens of behaviours at once — a decode resolves ~44
//! elements, each through its own rung of the source ladder — so the useful
//! question is not "how many things must I cover?" but "which behaviours does
//! this VIN prove?". Score every candidate by the set of behaviour *tokens* it
//! demonstrates, then greedily cover the union.
//!
//! Computed once when the artifact is built (see `ultravin-build`) and stored in
//! it, so callers get the cover for their exact data month at no runtime cost.
//!
//! Tokens have to be precise or the greedy quietly skips the case they were meant
//! to buy: `year` separates the two arithmetic year rules from the schema retry
//! that is the interesting one, and `conversion` carries the shape of the number
//! produced, not just which formula ran.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use crate::db::Db;
use crate::generate::{
    build_vin, pick_year_pub, schema_to_wmi, sweep, wmi_string, wmis_by_id, year_char, Dimension,
};

/// One behaviour a VIN demonstrates. Interned as a string so the whole grammar
/// stays one comparable, hashable type.
pub type Token = String;

/// Everything `result` proves about the decoder.
pub fn token_signature(
    vin: &str,
    result: &crate::DecodeResult<'_>,
    current_year: i32,
) -> HashSet<Token> {
    let mut t: HashSet<Token> = HashSet::new();
    let mut veh_type = String::new();
    let mut n_specs = 0usize;

    for e in &result.elements {
        let mut src: &str = &e.source;
        if let Some(id) = src
            .strip_prefix("Conversion ")
            .and_then(|s| s.split(':').next())
        {
            t.insert(format!("conversion|{id}|{}", number_shape(&e.value)));
            src = "Conversion";
        }
        if src == "Vehicle Specs" {
            n_specs += 1;
        }
        if e.element_id == 39 {
            veh_type = e.attribute_id.clone();
        }
        t.insert(format!("element|{}|{src}", e.element_id));
        t.insert(format!("value|{}|{}", e.element_id, !e.value.is_empty()));
        if !e.keys.is_empty() {
            let syntax: String = "|*#[-".chars().filter(|c| e.keys.contains(*c)).collect();
            t.insert(format!("keys|{syntax}|{}", e.keys.len()));
        }
    }
    t.insert(format!("specs|{}", (n_specs / 4).min(8)));

    let mut codes = result.error_codes.clone();
    codes.sort_unstable();
    for c in &codes {
        t.insert(format!("error|{c}"));
    }
    let combo: Vec<String> = codes.iter().map(|c| c.to_string()).collect();
    t.insert(format!("errors|{}", combo.join(",")));
    t.insert(format!("check_digit|{}", result.check_digit_valid));
    t.insert(format!("wmi_len|{}", result.wmi.len()));
    t.insert(format!("corrected|{}", !result.corrected_vin.is_empty()));

    let pos7_digit = vin.as_bytes().get(6).is_some_and(u8::is_ascii_digit);
    let car_lt = veh_type.parse::<i32>().is_ok_and(crate::tables::is_car_or_mpv);
    let conclusive = car_lt || raw_year(vin).is_some_and(|y| y > current_year + 2);
    t.insert(format!(
        "year|{}|{conclusive}",
        year_kind(vin, result.model_year, current_year, &veh_type)
    ));
    t.insert(format!("year_pass|{car_lt}|{pos7_digit}"));
    t
}

/// Decimal places, sign and zero-ness of a computed value — what a unit
/// conversion can get wrong without changing which formula ran.
fn number_shape(value: &str) -> String {
    if value.is_empty() {
        return "empty".to_string();
    }
    let decimals = value
        .split_once('.')
        .map_or(0, |(_, frac)| frac.len().min(12));
    let zero = value.parse::<f64>().map(|v| v == 0.0).unwrap_or(false);
    format!("{decimals}/{}/{zero}", value.starts_with('-'))
}

/// The year position 10 names outright, before any correction.
fn raw_year(vin: &str) -> Option<i32> {
    let c = *vin.as_bytes().get(9)? as char;
    (2010..2040).find(|y| year_char(*y) == c)
}

/// Which model-year path ran.
///
/// Three rules can move the year off what position 10 names, and only one is
/// interesting: the ±30 retry that fires when the WMI has no schema for the year
/// first computed. Predict the two arithmetic ones — a car/MPV with a digit in
/// position 7 is 30 years older, anything past `current + 2` is pulled back 30 —
/// and report `swap` only when the answer still disagrees. Vehicle type 3 needs
/// `trucktypeid`, which the result does not carry, so it is reported separately.
fn year_kind(
    vin: &str,
    model_year: Option<i32>,
    current_year: i32,
    veh_type: &str,
) -> &'static str {
    let (Some(my), Some(raw)) = (model_year, raw_year(vin)) else {
        return "none";
    };
    let pos7_digit = vin.as_bytes().get(6).is_some_and(u8::is_ascii_digit);
    if veh_type == "3" && pos7_digit {
        return "ambiguous";
    }
    let mut expected = if veh_type.parse::<i32>().is_ok_and(crate::tables::is_car_or_mpv)
        && pos7_digit
    {
        raw - 30
    } else {
        raw
    };
    if expected > current_year + 2 {
        expected -= 30;
    }
    if my == expected {
        "direct"
    } else {
        "swap"
    }
}

/// Candidate VINs for the cover: every sweep dimension, plus the cases that are
/// not rows in any table and have to be constructed.
///
/// Each candidate carries any token it proved *at construction time*. One tie is
/// invisible in decode output — that two patterns for an element reached
/// `cmp_keys_no_brackets` leaves no trace in the result — so without declaring it
/// here the greedy has no reason to keep the VIN built to cause it.
pub fn candidates(db: &Db, current_year: i32) -> Vec<(String, Option<Token>)> {
    let mut out: Vec<(String, Option<Token>)> = sweep(db, &Dimension::ALL, current_year)
        .into_iter()
        .map(|v| (v, None))
        .collect();
    // One token per element: a constructed tie can turn out not to tie for a
    // reason the construction cannot see, so keep one per element rather than
    // betting the whole branch on a single VIN.
    out.extend(
        dedup_tie_candidates(db, current_year)
            .into_iter()
            .map(|(v, eid)| (v, Some(format!("tie|dedup|{eid}")))),
    );
    out.extend(
        yearless_vspec_candidates(db, current_year)
            .into_iter()
            .map(|v| (v, Some("vspec|noyear".to_string()))),
    );
    out.extend(
        year_candidates(db, current_year)
            .into_iter()
            .map(|v| (v, None)),
    );
    out.extend(
        error_candidates(db, current_year)
            .into_iter()
            .map(|v| (v, None)),
    );
    out
}

/// Characters a `Keys` position accepts. `None` is "anything".
type Positions = Vec<Option<Vec<u8>>>;

fn charsets(keys: &str) -> Positions {
    let b = keys.as_bytes();
    let mut out: Positions = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'*' | b'_' => {
                out.push(None);
                i += 1;
            }
            b'[' => match b[i..].iter().position(|&c| c == b']') {
                Some(rel) => {
                    out.push(Some(class_members(&keys[i + 1..i + rel])));
                    i += rel + 1;
                }
                None => {
                    out.push(Some(vec![b[i]]));
                    i += 1;
                }
            },
            c => {
                out.push(Some(vec![c]));
                i += 1;
            }
        }
    }
    out
}

fn class_members(body: &str) -> Vec<u8> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if i + 2 < b.len() && b[i + 1] == b'-' {
            if b[i] <= b[i + 2] {
                out.extend(b[i]..=b[i + 2]);
            }
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// A `Keys` spec satisfying both `a` and `b`, or `None` when they cannot overlap.
///
/// This is what makes a deliberate tie constructible. Two patterns only reach a
/// tiebreak if one VIN matches both, and taking either pattern's own first
/// character usually excludes the other: `[AB]C` builds "AC", which `[BD]C`
/// rejects. Intersecting per position finds "BC", which both accept.
fn merge_keys(a: &str, b: &str) -> Option<String> {
    let (ca, cb) = (charsets(a), charsets(b));
    let mut merged = String::with_capacity(ca.len().max(cb.len()));
    for pos in 0..ca.len().max(cb.len()) {
        let both = match (ca.get(pos), cb.get(pos)) {
            (Some(Some(x)), Some(Some(y))) => {
                let hit: Vec<u8> = x.iter().copied().filter(|c| y.contains(c)).collect();
                Some(hit)
            }
            (Some(Some(x)), _) => Some(x.clone()),
            (_, Some(Some(y))) => Some(y.clone()),
            _ => None,
        };
        match both {
            None => merged.push('*'),
            Some(set) => merged.push(*set.first()? as char),
        }
    }
    Some(merged)
}

/// Does `var_keys` (VDS|VIS) satisfy this `Keys` spec?
fn keys_match(var_keys: &str, keys: &str) -> bool {
    let sets = charsets(keys);
    let v = var_keys.as_bytes();
    if v.len() < sets.len() {
        return false;
    }
    sets.iter()
        .zip(v)
        .all(|(set, &c)| set.as_ref().is_none_or(|s| s.contains(&c)))
}

fn len_no_star(keys: &str) -> usize {
    keys.bytes().filter(|&c| c != b'*').count()
}

/// VINs where two patterns for one element tie all the way down `dedup_cmp`, so
/// `cmp_keys_no_brackets` decides the winner.
///
/// The comparator orders by priority, then CreatedOn, then star-free key length,
/// and only then by the bracket-stripped keys. Reaching that last rung needs two
/// matched patterns for the same element agreeing on all three earlier terms —
/// and one VIN that matches both, which is what `merge_keys` constructs.
fn dedup_tie_candidates(db: &Db, current_year: i32) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let patterns = db.patterns();
    let mut start = 0usize;
    while start < patterns.len() && out.len() < 8 {
        let schema = patterns[start].vinschemaid.to_native();
        let mut end = start;
        while end < patterns.len() && patterns[end].vinschemaid.to_native() == schema {
            end += 1;
        }
        // Group this schema's patterns by the terms dedup_cmp compares first.
        // Only elements that actually reach dedup count: the pattern pass drops
        // 26/27/29/39 outright, exempt elements skip dedup, and an element with
        // no public Decode never becomes an item at all.
        let mut group: Vec<(i32, i64, usize, &str)> = patterns[start..end]
            .iter()
            .filter(|p| {
                let eid = p.elementid.to_native();
                !matches!(eid, 26 | 27 | 29 | 39)
                    && !crate::tables::is_exempt(eid)
                    && db
                        .element_by_id(eid)
                        .is_some_and(|e| e.decode_present && !e.isprivate)
            })
            .map(|p| {
                let keys = db.s(p.keys.to_native());
                (
                    p.elementid.to_native(),
                    p.createdon_key.to_native(),
                    len_no_star(keys),
                    keys,
                )
            })
            .collect();
        group.sort_unstable();
        for chunk in group.chunk_by(|a, b| (a.0, a.1, a.2) == (b.0, b.1, b.2)) {
            if let Some(vin) = tie_vin(db, schema, chunk, current_year) {
                out.push((vin, chunk[0].0));
                break;
            }
        }
        start = end;
    }
    out
}

fn tie_vin(
    db: &Db,
    schema: i32,
    group: &[(i32, i64, usize, &str)],
    current_year: i32,
) -> Option<String> {
    // Two plain literals of equal length can never both match one VIN; overlap
    // needs a class or a wildcard on at least two of them.
    let flexible = group
        .iter()
        .filter(|g| g.3.contains('[') || g.3.contains('*'))
        .count();
    if group.len() < 2 || flexible < 2 {
        return None;
    }
    let (wmi, yearfrom, yearto) = wmi_for_schema(db, schema)?;
    let year = crate::generate::pick_year_pub(yearfrom, yearto, current_year);
    for i in 0..group.len().min(24) {
        for j in (i + 1)..group.len().min(24) {
            let Some(merged) = merge_keys(group[i].3, group[j].3) else {
                continue;
            };
            let Some(vin) = build_vin(wmi, &merged, year) else {
                continue;
            };
            let var_keys = format!("{}|{}", &vin[3..8], &vin[9..17]);
            if keys_match(&var_keys, group[i].3) && keys_match(&var_keys, group[j].3) {
                return Some(vin);
            }
        }
    }
    None
}

fn wmi_for_schema(db: &Db, schema: i32) -> Option<(&str, i32, i32)> {
    db.wmis().iter().find_map(|w| {
        db.wmi_vinschema_for(w.id.to_native())
            .iter()
            .find(|l| l.vinschemaid.to_native() == schema)
            .map(|l| {
                (
                    db.s(w.wmi.to_native()),
                    l.yearfrom.to_native(),
                    l.yearto_or(9999),
                )
            })
    })
}

/// VINs for vehicle-spec schemas carrying no year rows.
///
/// `append_vehicle_specs` treats "no year rows" as matching every year — a
/// different arm from the usual exact-year test, and only six schemas in the
/// data can reach it. Nothing in the decode output distinguishes the arm, so the
/// candidate declares it.
fn yearless_vspec_candidates(db: &Db, current_year: i32) -> Vec<String> {
    let index = schema_to_wmi(db);
    let mut out = Vec::new();
    for vs in db.vspecschemas() {
        if !db.vspecschema_years_for(vs.id.to_native()).is_empty() {
            continue;
        }
        let Some(m) = db.vspecschema_models_for(vs.id.to_native()).first() else {
            continue;
        };
        let modelid = m.modelid.to_native().to_string();
        let hit = db
            .patterns()
            .iter()
            .find(|p| p.elementid.to_native() == 28 && db.s(p.attributeid.to_native()) == modelid);
        let Some(p) = hit else { continue };
        let Ok(i) = index.binary_search_by_key(&p.vinschemaid.to_native(), |e| e.0) else {
            continue;
        };
        let e = index[i];
        let wmis = wmis_by_id(db);
        if let Some(wmi) = wmi_string(&wmis, e.1) {
            out.extend(build_vin(
                wmi,
                db.s(p.keys.to_native()),
                pick_year_pub(e.2, e.3, current_year),
            ));
        }
    }
    out
}

/// VINs that force the ±30 schema retry: a car/MPV WMI whose schemas all end by
/// 1998, given the year character for `yearto + 30`. The 1998 bound matters — a
/// later year would exceed `current + 2` and be pulled back by the future-year
/// correction instead, which never reaches the retry.
fn year_candidates(db: &Db, current_year: i32) -> Vec<String> {
    let mut out = Vec::new();
    for w in db.wmis() {
        if !crate::tables::is_car_or_mpv(w.vehicletypeid.to_native())
            || db.s(w.wmi.to_native()).len() != 3
        {
            continue;
        }
        let links = db.wmi_vinschema_for(w.id.to_native());
        if links.is_empty() {
            continue;
        }
        let latest = links
            .iter()
            .map(|l| l.yearto.to_native())
            .max()
            .unwrap_or(0);
        if !(1980..=1998).contains(&latest) {
            continue;
        }
        let Some(built) = build_vin(db.s(w.wmi.to_native()), "*****", latest + 30) else {
            continue;
        };
        let mut vin: Vec<u8> = built.into_bytes();
        vin[6] = b'A'; // alphabetic position 7 keeps the year conclusive
        vin[8] = b'0';
        let text = String::from_utf8_lossy(&vin).into_owned();
        vin[8] = match crate::check_digit(&text) {
            Some(c) if c != '?' => c as u8,
            _ => b'0',
        };
        out.push(String::from_utf8_lossy(&vin).into_owned());
        if out.len() >= 8 {
            break;
        }
    }
    let _ = current_year;
    out
}

/// VINs aimed at each reachable error code. Code 12 needs a caller-supplied
/// model year, which the decode API has no way to accept, so it is unreachable.
fn error_candidates(db: &Db, current_year: i32) -> Vec<String> {
    let Some(w) = db
        .wmis()
        .iter()
        .find(|w| db.s(w.wmi.to_native()).len() == 3)
    else {
        return Vec::new();
    };
    let real = db.s(w.wmi.to_native());
    // The lowest-sorted 3-char WMI has no I/O/Q, so this builds; the deliberate
    // invalid-character case below is injected by `swap`, not by `build_vin`.
    let Some(base) = build_vin(real, "*****", current_year.min(2020)) else {
        return Vec::new();
    };
    let swap = |i: usize, c: char| {
        let mut b = base.clone().into_bytes();
        b[i] = c as u8;
        String::from_utf8_lossy(&b).into_owned()
    };
    let mut out = vec![
        base.clone(),
        swap(8, if base.as_bytes()[8] == b'1' { '2' } else { '1' }), // bad check digit
        base[..11].to_string(),                                      // incomplete
        real.to_string(),                                            // no descriptor at all
        format!("ZZZ{}", &base[3..]),                                // unknown WMI
        format!("{real}00000000000000"),                             // valid WMI, no pattern
        swap(8, 'I'),                                                // invalid character
        swap(9, 'U'),                                                // invalid year character
    ];
    // One-position corruptions drive the suggested-VIN correction ladder.
    for pos in [4usize, 6, 12] {
        out.push(swap(
            pos,
            if base.as_bytes()[pos] == b'B' {
                'C'
            } else {
                'B'
            },
        ));
    }
    out
}

/// Greedy set cover over token signatures: repeatedly take the candidate adding
/// the most uncovered tokens.
///
/// Lazy (CELF) evaluation — a stale gain is re-checked before use — so the scan
/// is near-linear rather than quadratic. Greedy is the best any polynomial
/// algorithm achieves on set cover (`ln n`, and no better unless P = NP); ties
/// break on index, so the result is deterministic.
pub fn greedy_cover(sigs: &[HashSet<Token>]) -> Vec<usize> {
    // Max-heap on gain; `Reverse` on the index makes the lowest index win a tie,
    // so the same input always yields the same cover.
    let mut heap: BinaryHeap<(usize, Reverse<usize>)> = sigs
        .iter()
        .enumerate()
        .map(|(i, s)| (s.len(), Reverse(i)))
        .collect();

    let mut covered: HashSet<Token> = HashSet::new();
    let mut chosen = Vec::new();
    while let Some((stale, Reverse(i))) = heap.pop() {
        let gain = sigs[i].difference(&covered).count();
        if gain == 0 {
            continue;
        }
        // The cached gain was optimistic and someone else now looks better:
        // re-queue at the true gain rather than taking this one.
        if gain < stale && heap.peek().is_some_and(|&(next, _)| gain < next) {
            heap.push((gain, Reverse(i)));
            continue;
        }
        covered.extend(sigs[i].iter().cloned());
        chosen.push(i);
    }
    chosen
}

/// Compute the cover for `db`: sweep every candidate, score it, minimise.
///
/// This decodes every candidate (~584k), so it belongs at artifact build time,
/// not in a request path.
pub fn compute(db: &Db, current_year: i32) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut sigs: Vec<HashSet<Token>> = Vec::new();
    let mut universe: HashSet<Token> = HashSet::new();

    // Keep a candidate only when it widens the universe. That cannot shrink the
    // universe — a VIN adding nothing leaves it unchanged — so greedy over the
    // survivors still covers everything, from thousands instead of half a million.
    for (vin, declared) in candidates(db, current_year) {
        let result = crate::decode_with(db, &vin, i64::MAX, current_year);
        let mut sig = token_signature(&vin, &result, current_year);
        sig.extend(declared);
        if !sig.is_subset(&universe) {
            universe.extend(sig.iter().cloned());
            kept.push(vin);
            sigs.push(sig);
        }
    }
    greedy_cover(&sigs)
        .into_iter()
        .map(|i| kept[i].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(tokens: &[&str]) -> HashSet<Token> {
        tokens.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn greedy_takes_the_dominating_candidate_alone() {
        let sigs = vec![
            sig(&["a", "b"]),
            sig(&["b", "c"]),
            sig(&["a", "b", "c"]),
            sig(&["a"]),
        ];
        assert_eq!(greedy_cover(&sigs), vec![2]);
    }

    #[test]
    fn greedy_covers_everything_and_repeats() {
        let sigs = vec![sig(&["a"]), sig(&["b"]), sig(&["c", "a"]), sig(&["b", "c"])];
        let first = greedy_cover(&sigs);
        assert_eq!(greedy_cover(&sigs), first);
        let mut covered: HashSet<Token> = HashSet::new();
        for i in &first {
            covered.extend(sigs[*i].iter().cloned());
        }
        assert_eq!(covered, sig(&["a", "b", "c"]));
    }

    #[test]
    fn number_shape_separates_precision_and_sign() {
        assert_eq!(number_shape("2998.832712"), "6/false/false");
        assert_eq!(number_shape("-1.5"), "1/true/false");
        assert_eq!(number_shape("0"), "0/false/true");
        assert_eq!(number_shape(""), "empty");
    }

    #[test]
    fn merge_keys_intersects_character_classes() {
        // Taking either pattern's own first character excludes the other; the
        // overlapping member is the only VIN that reaches the tiebreak.
        assert_eq!(merge_keys("[AB]C", "[BD]C").as_deref(), Some("BC"));
        assert_eq!(merge_keys("A*C", "*BC").as_deref(), Some("ABC"));
        assert_eq!(merge_keys("AC", "BC"), None);
        assert_eq!(merge_keys("A|B", "A|C"), None);
    }

    #[test]
    fn charsets_expand_ranges_and_reject_reversed_ones() {
        assert_eq!(class_members("A-C"), b"ABC".to_vec());
        assert_eq!(class_members("C-A"), Vec::<u8>::new());
        assert!(keys_match("CM826|3A004352", "[C-F]M826"));
        assert!(!keys_match("CM826|3A004352", "[D-F]M826"));
    }

    #[test]
    fn year_kind_predicts_the_arithmetic_rules() {
        // Position 7 is 'A': no carLT shift, so 2020 is the year at face value.
        assert_eq!(
            year_kind("1HGCM8A63LA004352", Some(2020), 2026, "2"),
            "direct"
        );
        assert_eq!(
            year_kind("1HGCM8A63LA004352", Some(1990), 2026, "2"),
            "swap"
        );
        // Position 7 is '2', a digit: a car/MPV is 30 years older, still no retry.
        assert_eq!(
            year_kind("1HGCM8263LA004352", Some(1990), 2026, "2"),
            "direct"
        );
        assert_eq!(
            year_kind("1HGCM8263LA004352", Some(2020), 2026, "3"),
            "ambiguous"
        );
    }
}
