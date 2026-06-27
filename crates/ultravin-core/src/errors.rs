//! Error-code accumulation: the W1 subset (0,1,6,7,8,9,10,11,400,14) of the
//! `spvindecode_core` `ReturnCode` logic, plus the character-validity scan.

use std::collections::BTreeSet;

use crate::checkdigit::check_digit_with_flag;
use crate::db::Db;
use crate::decode::CoreResult;
use crate::tables::Wmi;

/// element-5 (Body Style) attribute ids that flag an off-road PIN (code 10).
const OFF_ROAD: [&str; 10] = [
    "69", "84", "86", "88", "97", "105", "113", "124", "126", "127",
];

/// Computed error state for a decode.
pub struct ErrorState {
    /// Sorted error codes (the element-143 CSV / `error_codes` list).
    pub codes: Vec<i32>,
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

/// `true` if any character is invalid at its position (code 400).
fn has_invalid_chars(vin: &str, start_pos: usize, skip9: bool) -> bool {
    let b = vin.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        let j = i + 1; // 1-based
        if j == 9 && skip9 {
            continue;
        }
        let bad = if j == 9 {
            !class_cd(c)
        } else {
            (j < start_pos && !class1(c))
                || (j >= start_pos && !class_digit(c))
                || (j == 10 && !class_my(c))
        };
        if bad {
            return true;
        }
    }
    false
}

/// Compute the W1 error codes for a decode.
pub fn compute_errors(
    db: &Db,
    vin: &str,
    core: &CoreResult,
    model_year: Option<i32>,
    wmi: Option<&Wmi>,
) -> ErrorState {
    let items = &core.items;
    let is_off_road = items
        .iter()
        .any(|it| it.element_id == 5 && OFF_ROAD.contains(&it.attribute_id.as_str()));
    let is_engine_off_road = items
        .iter()
        .any(|it| it.element_id == 5 && it.attribute_id == "64");
    let is_vin_exception = db.vinexception_checkdigit(vin);
    let (start_pos, is_car_mpv_lt) = start_context(vin, wmi);

    let mut raw: BTreeSet<i32> = BTreeSet::new();
    if !core.wmi_found {
        raw.insert(7);
    } else if core.pattern_count == 0 {
        raw.insert(8);
    }
    if is_engine_off_road {
        raw.insert(9);
    }
    if is_off_road {
        raw.insert(10);
    }
    if model_year.is_none() {
        raw.insert(11);
    }

    let skip9 = is_off_road || is_vin_exception;
    if has_invalid_chars(vin, start_pos, skip9) {
        raw.insert(400);
    }

    // Check digit (code 1) when the VIN is full length.
    let mut check_digit_valid = false;
    if vin.len() < 17 {
        raw.insert(6);
    } else if let Some(calc) = check_digit_with_flag(vin, is_car_mpv_lt) {
        check_digit_valid = vin.as_bytes()[8] == calc as u8;
        if !check_digit_valid && !is_vin_exception {
            raw.insert(1);
        }
    }

    // Clean rule: removing 9/10/12, if nothing remains add code 0.
    let clean_empty = raw.iter().all(|c| matches!(c, 9 | 10 | 12));
    if clean_empty {
        raw.insert(0);
    }
    // Code 14: clean VIN with no Model element.
    let has_model = items.iter().any(|it| it.element_id == 28);
    if raw.contains(&0) && !has_model {
        raw.insert(14);
    }

    ErrorState {
        codes: raw.into_iter().collect(),
        is_off_road,
        is_vin_exception,
        check_digit_valid,
    }
}
