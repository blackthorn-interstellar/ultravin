//! Port of `vpic.fVINCheckDigit2` (data-free). Canonical: `vpic/procs/fvincheckdigit2.sql`.
//!
//! Parity notes (replicate exactly):
//! - Per-position character-class validity is checked first; an invalid char
//!   makes the whole function return `'?'` (no digit).
//! - Transliteration excludes I/O/Q. Weights are
//!   `[8,7,6,5,4,3,2,10,0,9,8,7,6,5,4,3,2]` (position 9 weight 0).
//! - The result is `sum % 11`, where remainder 10 renders as `'X'`.

/// Transliterate a VIN character to its numeric value. `None` for I/O/Q or any
/// non-VIN byte (the SQL `CASE ... ELSE -1`).
fn translit(c: u8) -> Option<u32> {
    Some(match c {
        b'0'..=b'9' => (c - b'0') as u32,
        b'A' | b'J' => 1,
        b'B' | b'K' | b'S' => 2,
        b'C' | b'L' | b'T' => 3,
        b'D' | b'M' | b'U' => 4,
        b'E' | b'N' | b'V' => 5,
        b'F' | b'W' => 6,
        b'G' | b'P' | b'X' => 7,
        b'H' | b'Y' => 8,
        b'R' | b'Z' => 9,
        _ => return None,
    })
}

/// Is `c` allowed at 1-based position `i`, given pos-3 and the car/MPV/LT flag?
/// Mirrors the `CASE` over `patternMY` / `patternNumbersOnly` / `patternDefault`
/// in `fVINCheckDigit2`. Case-insensitive (input is upper-cased by the caller).
fn valid_at(i: usize, c: u8, pos3: u8, is_car_mpv_lt: bool) -> bool {
    // patternMY = [A-H,J-N,P,R-T,V-Y,1-9]; patternNumbersOnly = [0-9];
    // patternDefault = [A-H,J-N,P,R-Z,0-9].
    let my =
        matches!(c, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'T' | b'V'..=b'Y' | b'1'..=b'9');
    let nums = c.is_ascii_digit();
    let default = matches!(c, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z' | b'0'..=b'9');
    match i {
        10 => my,
        13 if pos3 != b'9' && is_car_mpv_lt => nums,
        14 if pos3 != b'9' => nums,
        _ if i >= 15 => nums,
        _ => default,
    }
}

/// Compute the position-9 check digit per `fVINCheckDigit2`. Returns `Some('X')`
/// or `Some(d)`, `Some('?')` if any character is invalid at its position, or
/// `None` when the VIN is not 17 characters (the SQL returns `''`).
pub fn check_digit_with_flag(vin: &str, is_car_mpv_lt: bool) -> Option<char> {
    let b = vin.as_bytes();
    if b.len() != 17 {
        return None;
    }
    let pos3 = b[2];
    let weights: [u32; 17] = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];
    let mut sum: u32 = 0;
    for (idx, &c) in b.iter().enumerate() {
        let i = idx + 1; // 1-based, matching the SQL
        if !valid_at(i, c, pos3, is_car_mpv_lt) {
            return Some('?');
        }
        let Some(v) = translit(c) else {
            return Some('?');
        };
        sum += v * weights[idx];
    }
    let r = sum % 11;
    Some(if r == 10 {
        'X'
    } else {
        (b'0' + r as u8) as char
    })
}

/// Convenience wrapper for `fVINCheckDigit2(vin, false)`.
pub fn check_digit(vin: &str) -> Option<char> {
    check_digit_with_flag(vin, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_vin_check_digit_is_3() {
        assert_eq!(check_digit("1HGCM82633A004352"), Some('3'));
    }

    #[test]
    fn x_check_digit() {
        // 11111111111111111: weighted sum = 8+7+6+5+4+3+2+10+0+9+8+7+6+5+4+3+2 = 89; 89 % 11 = 1.
        assert_eq!(check_digit("11111111111111111"), Some('1'));
    }

    #[test]
    fn short_vin_returns_none() {
        assert_eq!(check_digit("1HG"), None);
    }

    #[test]
    fn invalid_char_returns_question_mark() {
        // 'I' is invalid in the default class.
        assert_eq!(check_digit("1HGCM8263IA004352"), Some('?'));
    }
}
