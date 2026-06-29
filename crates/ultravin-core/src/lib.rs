//! ultravin-core — pure-Rust NHTSA vPIC VIN decoder engine.
//!
//! W1: a working first-pass decode against the embedded rkyv artifact — WMI
//! lookup, schema/pattern matching, layered sources, per-element dedup, element
//! resolution, model year, and the basic error codes. Byte-for-byte parity with
//! the official Postgres `vpic.spvindecode` is the long-term goal; the 4-pass
//! best-of, Conversion/Vehicle-Specs sources, and suggested-VIN are W2.

mod checkdigit;
mod conversion;
pub mod db;
mod decode;
mod errors;
mod hash;
mod matcher;
mod resolve;
pub mod tables;
mod wmi;
mod year;

use std::time::{SystemTime, UNIX_EPOCH};

pub use checkdigit::check_digit;
pub use db::Db;
pub use matcher::sqlwild_to_regex;
pub use wmi::{vin_descriptor, vin_wmi};

/// One resolved output element (the 15-column `spvindecode` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedElement {
    pub group_name: String,
    pub variable: String,
    pub value: String,
    pub element_id: i32,
    pub attribute_id: String,
    pub code: String,
    pub data_type: String,
    pub decode: String,
    pub source: String,
    pub pattern_id: Option<i32>,
    pub vin_schema_id: Option<i32>,
    pub keys: String,
    pub created_on: Option<i64>,
    pub wmi_id: Option<i32>,
    pub to_be_qced: bool,
}

/// A decoded VIN result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeResult {
    pub vin: String,
    pub wmi: String,
    pub descriptor: String,
    pub model_year: Option<i32>,
    pub error_codes: Vec<i32>,
    pub check_digit_valid: bool,
    pub corrected_vin: String,
    pub elements: Vec<DecodedElement>,
}

fn opt_i32(v: i32) -> Option<i32> {
    if v == tables::NULL_I32 {
        None
    } else {
        Some(v)
    }
}

fn opt_i64(v: i64) -> Option<i64> {
    if v == tables::NULL_I64 {
        None
    } else {
        Some(v)
    }
}

fn scrub(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

/// Convert Unix epoch seconds to the calendar year (Hinnant's civil algorithm).
fn epoch_to_year(secs: i64) -> i32 {
    let days = secs.div_euclid(86400);
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }) as i32
}

/// Decode a VIN using the embedded database and the system clock.
pub fn decode(input: &str) -> DecodeResult {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    decode_with(Db::embedded(), input, secs * 1_000_000, epoch_to_year(secs))
}

/// Decode a VIN against an explicit database and clock (injectable for tests),
/// with no caller-supplied model year.
pub fn decode_with(db: &Db, input: &str, now_micros: i64, current_year: i32) -> DecodeResult {
    decode_full(db, input, now_micros, current_year, None)
}

/// Decode many VINs in parallel over the shared (immutable) embedded archive.
///
/// The clock is read once so a batch is internally consistent; each VIN is then
/// decoded independently via [`decode_with`] across rayon's thread pool. Output
/// order matches `inputs`. Per-VIN output is identical to calling [`decode`].
pub fn decode_batch(inputs: &[String]) -> Vec<DecodeResult> {
    use rayon::prelude::*;

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_micros = secs * 1_000_000;
    let year = epoch_to_year(secs);
    let db = Db::embedded();
    inputs
        .par_iter()
        .map(|v| decode_with(db, v, now_micros, year))
        .collect()
}

/// One decode pass (a single `spvindecode_core` invocation): its items (with the
/// 142/143/144/156/191/196 corrections appended, values still pre-resolution) and
/// the metadata the scorer and result need.
struct Pass {
    id: i32,
    model_year: Option<i32>,
    items: Vec<decode::DecodingItem>,
    codes: Vec<i32>,
    corrected_vin: String,
    check_digit_valid: bool,
}

/// The full wrapper (`vpic.spvindecode`): up to 4 best-of passes, scoring, and
/// the GroupName-ordered projection. `caller_year` is the optional caller MY.
pub fn decode_full(
    db: &Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
    caller_year: Option<i32>,
) -> DecodeResult {
    let vin = input.trim().to_ascii_uppercase();
    let var_wmi = vin_wmi(&vin);
    let descriptor = vin_descriptor(&vin);
    let var_keys = decode::build_var_keys(&vin);

    let v_limit = current_year + 2;
    let plan = year::resolve_years(&vin, &var_wmi, db, current_year);

    // Pass 1 (descriptor/dmy) is permanently dead in the proc — skipped here.
    let mut passes: Vec<Pass> = Vec::new();
    let mut model_year_source = "***X*|Y".to_string();
    let mut do3and4 = true;

    // Pass 2: caller year, only when in [1980, v_limit] and not already a candidate.
    if let Some(yc) = caller_year {
        if (1980..=v_limit).contains(&yc) {
            if Some(yc) == plan.rmy || Some(yc) == plan.omy {
                do3and4 = true;
            } else {
                model_year_source = yc.to_string();
                let p = run_pass(
                    db,
                    &vin,
                    &var_wmi,
                    &var_keys,
                    now_micros,
                    &descriptor,
                    2,
                    Some(yc),
                    &model_year_source,
                    true,
                    true,
                );
                do3and4 = p.codes.contains(&8) && plan.rmy.is_some();
                passes.push(p);
            }
        }
    }

    if do3and4 {
        // Pass 3: rmy.
        let e12 = caller_year.is_some() && plan.rmy.is_some() && caller_year != plan.rmy;
        passes.push(run_pass(
            db,
            &vin,
            &var_wmi,
            &var_keys,
            now_micros,
            &descriptor,
            3,
            plan.rmy,
            &model_year_source,
            plan.conclusive,
            e12,
        ));
        // Pass 4: omy (only when inconclusive).
        if let Some(omy) = plan.omy {
            let e12 = caller_year.is_some() && caller_year != Some(omy);
            passes.push(run_pass(
                db,
                &vin,
                &var_wmi,
                &var_keys,
                now_micros,
                &descriptor,
                4,
                Some(omy),
                &model_year_source,
                plan.conclusive,
                e12,
            ));
        }
    }

    let best_id = best_pass(&passes, db, caller_year);
    let best = passes
        .into_iter()
        .find(|p| p.id == best_id)
        .expect("at least one pass ran");

    let mut items = best.items;
    // QC override + TobeQCed delete (inert with current data) then XXX resolution.
    items.retain(|it| !it.to_be_qced);
    resolve::resolve_xxx(db, &mut items);

    let elements = project(db, &items);

    DecodeResult {
        vin,
        wmi: var_wmi,
        descriptor,
        model_year: best.model_year,
        error_codes: best.codes,
        check_digit_valid: best.check_digit_valid,
        corrected_vin: best.corrected_vin,
        elements,
    }
}

/// Run one `spvindecode_core` pass and append its corrections.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    db: &Db,
    vin: &str,
    var_wmi: &str,
    var_keys: &str,
    now_micros: i64,
    descriptor: &str,
    id: i32,
    model_year: Option<i32>,
    model_year_source: &str,
    conclusive: bool,
    error12: bool,
) -> Pass {
    let core = decode::decode_core(
        db,
        var_wmi,
        var_keys,
        model_year,
        model_year_source,
        now_micros,
    );
    let err = errors::compute_errors(db, vin, var_wmi, &core, model_year, error12, conclusive);

    let mut items = core.items;
    let codes_csv = err
        .codes
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let error_text = error_messages(db, &err);
    append_correction(&mut items, 142, &err.corrected_vin);
    append_correction(&mut items, 143, &codes_csv);
    append_correction(&mut items, 144, &err.error_bytes);
    append_correction(&mut items, 156, &err.additional_info);
    append_correction(&mut items, 191, &error_text);
    append_correction(&mut items, 196, descriptor);

    Pass {
        id,
        model_year,
        items,
        codes: err.codes,
        corrected_vin: err.corrected_vin,
        check_digit_valid: err.check_digit_valid,
    }
}

/// Pick the best pass by the `x` scoring table: ErrorValue desc, ElementsWeight
/// desc, Patterns desc, ModelYear desc (NULLs last), then lowest pass id.
fn best_pass(passes: &[Pass], db: &Db, caller_year: Option<i32>) -> i32 {
    passes
        .iter()
        .max_by(|a, b| {
            let sa = score(a, db, caller_year);
            let sb = score(b, db, caller_year);
            // a is "greater" (preferred) when its tuple ranks higher.
            sa.0.cmp(&sb.0)
                .then(sa.1.cmp(&sb.1))
                .then(sa.2.cmp(&sb.2))
                .then(cmp_year_nulls_last(sa.3, sb.3))
                .then(b.id.cmp(&a.id)) // lower id wins ties
        })
        .map(|p| p.id)
        .unwrap_or(0)
}

/// (ErrorValue, ElementsWeight, Patterns, ModelYear+bonus) for a pass.
fn score(pass: &Pass, db: &Db, caller_year: Option<i32>) -> (i32, i32, i32, Option<i32>) {
    let error_value: i32 = pass
        .codes
        .iter()
        .map(|c| tables::errorcode_weight(*c))
        .sum();

    let mut weighted: hash::IntSet<i32> = hash::IntSet::default();
    for it in &pass.items {
        if !it.value.is_empty() {
            weighted.insert(it.element_id);
        }
    }
    let elements_weight: i32 = weighted
        .iter()
        .filter_map(|eid| db.element_by_id(*eid))
        .map(|e| e.weight.to_native())
        .filter(|w| *w != tables::NULL_I32)
        .sum();

    let patterns = pass
        .items
        .iter()
        .filter(|it| {
            matches!(
                it.source.as_ref(),
                "Pattern" | "EngineModelPattern" | "Formula Pattern"
            ) && !it.value.is_empty()
                && it.value != "Not Applicable"
        })
        .count() as i32;

    let model_year = pass
        .items
        .iter()
        .find(|it| it.element_id == 29)
        .and_then(|it| it.value.parse::<i32>().ok())
        .map(|y| y + if caller_year == Some(y) { 10000 } else { 0 });

    (error_value, elements_weight, patterns, model_year)
}

/// DESC ordering with NULLs last: `Some` always beats `None`.
fn cmp_year_nulls_last(a: Option<i32>, b: Option<i32>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Project the surviving items into output elements (non-empty Decode, public),
/// ordered by the GroupName CASE rank then element id.
fn project(db: &Db, items: &[decode::DecodingItem]) -> Vec<DecodedElement> {
    let mut elements: Vec<DecodedElement> = Vec::with_capacity(items.len());
    for it in items {
        let Some(e) = db.element_by_id(it.element_id) else {
            continue;
        };
        let decode_str = db.s(e.decode.to_native());
        if !e.decode_present || decode_str.is_empty() || e.isprivate {
            continue;
        }
        elements.push(DecodedElement {
            group_name: db.s(e.groupname.to_native()).to_string(),
            variable: db.s(e.name.to_native()).to_string(),
            value: scrub(&it.value),
            element_id: it.element_id,
            attribute_id: it.attribute_id.clone(),
            code: db.s(e.code.to_native()).to_string(),
            data_type: db.s(e.datatype.to_native()).to_string(),
            decode: decode_str.to_string(),
            source: it.source.to_string(),
            pattern_id: opt_i32(it.pattern_id),
            vin_schema_id: opt_i32(it.vin_schema_id),
            keys: it.keys.clone(),
            created_on: opt_i64(it.created_on),
            wmi_id: opt_i32(it.wmi_id),
            to_be_qced: it.to_be_qced,
        });
    }
    // GroupName CASE rank, then element id (the proc leaves intra-group order to
    // the scan; element id is the deterministic, stable secondary key). A cached
    // key keeps `group_rank` to one call per element and sorts in place — no tuple
    // Vec and no second pass to strip the rank. `sort_by_cached_key` is stable, so
    // the few duplicate exempt elements keep insertion order, exactly as before.
    elements.sort_by_cached_key(|e| (tables::group_rank(&e.group_name), e.element_id));
    elements
}

/// Build the element-191 error text: error-code names joined by `; `.
fn error_messages(db: &Db, err: &errors::ErrorState) -> String {
    let tag = tables::element_lookup_tag(143);
    let mut parts: Vec<String> = Vec::new();
    for &code in &err.codes {
        let Some(t) = tag else { break };
        let Some(name) = db.lookup(t, code) else {
            continue;
        };
        let mut name = name.trim().to_string();
        if err.is_off_road && code == 1 {
            name.push_str(
                " NOTE: Disregard if this is an off-road vehicle PIN, as check digit calculation may not be accurate.",
            );
        }
        if err.is_vin_exception && code == 0 {
            name.push_str(
                " NOTE: Check Digit Exception - The check digit was given an exception based on data from the OEM indicating an error on production.",
            );
        }
        parts.push(name);
    }
    // `left(errorMessages, 500)` counts CHARACTERS, not bytes; multi-byte chars
    // (e.g. the en-dash in the code-10 message) must not be split mid-codepoint.
    parts.join("; ").chars().take(500).collect()
}

fn append_correction(items: &mut Vec<decode::DecodingItem>, element_id: i32, value: &str) {
    items.push(decode::DecodingItem {
        created_on: tables::NULL_I64,
        pattern_id: tables::NULL_I32,
        keys: String::new(),
        vin_schema_id: tables::NULL_I32,
        wmi_id: tables::NULL_I32,
        element_id,
        attribute_id: value.to_string(),
        value: std::borrow::Cow::Owned(value.to_string()),
        source: std::borrow::Cow::Borrowed("Corrections"),
        priority: 999,
        to_be_qced: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> &'static Db {
        Db::embedded()
    }

    #[test]
    fn check_digit_helper_still_works() {
        assert_eq!(check_digit("1HGCM82633A004352"), Some('3'));
    }

    #[test]
    fn canonical_honda_decodes() {
        let d = db();
        if !d.is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        let r = decode_with(d, "1HGCM82633A004352", 1_750_000_000_000_000, 2026);
        let get = |eid: i32| r.elements.iter().find(|e| e.element_id == eid);
        assert_eq!(get(26).map(|e| e.value.as_str()), Some("HONDA"));
        assert_eq!(get(28).map(|e| e.value.as_str()), Some("Accord"));
        assert_eq!(r.model_year, Some(2003));
        assert_eq!(get(18).map(|e| e.value.as_str()), Some("J30A4"));
        assert_eq!(get(39).map(|e| e.value.as_str()), Some("PASSENGER CAR"));
        assert_eq!(r.error_codes, vec![0]);
    }
}
