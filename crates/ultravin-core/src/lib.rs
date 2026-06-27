//! ultravin-core — pure-Rust NHTSA vPIC VIN decoder engine.
//!
//! W1: a working first-pass decode against the embedded rkyv artifact — WMI
//! lookup, schema/pattern matching, layered sources, per-element dedup, element
//! resolution, model year, and the basic error codes. Byte-for-byte parity with
//! the official Postgres `vpic.spvindecode` is the long-term goal; the 4-pass
//! best-of, Conversion/Vehicle-Specs sources, and suggested-VIN are W2.

mod checkdigit;
pub mod db;
mod decode;
mod errors;
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

/// Decode a VIN against an explicit database and clock (injectable for tests).
pub fn decode_with(db: &Db, input: &str, now_micros: i64, current_year: i32) -> DecodeResult {
    let vin = input.trim().to_ascii_uppercase();
    let var_wmi = vin_wmi(&vin);
    let descriptor = vin_descriptor(&vin);

    let yc = year::choose_model_year(&vin, db, current_year);
    let var_keys = decode::build_var_keys(&vin);

    let core = decode::decode_core(
        db,
        &var_wmi,
        &var_keys,
        yc.model_year,
        &yc.source,
        now_micros,
    );

    let wmi_row = db.wmi_by_str(&var_wmi, now_micros);
    let err = errors::compute_errors(db, &vin, &core, yc.model_year, wmi_row);

    let mut items = core.items;
    resolve::resolve_xxx(db, &mut items);

    // Corrections pseudo-elements (142,143,144,156,191,196).
    let codes_csv = err
        .codes
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let error_text = error_messages(db, &err);
    append_correction(&mut items, 142, "");
    append_correction(&mut items, 143, &codes_csv);
    append_correction(&mut items, 144, "");
    append_correction(&mut items, 156, "");
    append_correction(&mut items, 191, &error_text);
    append_correction(&mut items, 196, &descriptor);

    // Project items -> output elements (element decode present, non-empty, public).
    let mut elements: Vec<DecodedElement> = Vec::new();
    for it in &items {
        let Some(e) = db.element_by_id(it.element_id) else {
            continue;
        };
        let decode_str = db.s(e.decode);
        if !e.decode_present || decode_str.is_empty() || e.isprivate {
            continue;
        }
        elements.push(DecodedElement {
            group_name: db.s(e.groupname).to_string(),
            variable: db.s(e.name).to_string(),
            value: scrub(&it.value),
            element_id: it.element_id,
            attribute_id: it.attribute_id.clone(),
            code: db.s(e.code).to_string(),
            data_type: db.s(e.datatype).to_string(),
            decode: decode_str.to_string(),
            source: it.source.clone(),
            pattern_id: opt_i32(it.pattern_id),
            vin_schema_id: opt_i32(it.vin_schema_id),
            keys: it.keys.clone(),
            created_on: opt_i64(it.created_on),
            wmi_id: opt_i32(it.wmi_id),
            to_be_qced: it.to_be_qced,
        });
    }
    // W1 orders by element id (GroupName ordering is W2). Stable: exempt dups keep order.
    elements.sort_by_key(|e| e.element_id);

    let corrected_vin = err.corrected_vin_w1();
    DecodeResult {
        vin,
        wmi: var_wmi,
        descriptor,
        model_year: yc.model_year,
        error_codes: err.codes,
        check_digit_valid: err.check_digit_valid,
        corrected_vin,
        elements,
    }
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
    let mut s = parts.join("; ");
    s.truncate(500);
    s
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
        value: value.to_string(),
        source: "Corrections".to_string(),
        priority: 999,
        to_be_qced: false,
    });
}

impl errors::ErrorState {
    /// W1 suggested/corrected VIN is always empty (W2).
    fn corrected_vin_w1(&self) -> String {
        String::new()
    }
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
