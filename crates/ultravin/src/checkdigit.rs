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
const fn translit(c: u8) -> Option<u32> {
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

/// The model-year character class `patternMY` = `[A-H,J-N,P,R-T,V-Y,1-9]`.
pub(crate) const fn is_my_char(c: u8) -> bool {
    matches!(c, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'T' | b'V'..=b'Y' | b'1'..=b'9')
}

/// The default character class `patternDefault` = `[A-H,J-N,P,R-Z,0-9]`.
pub(crate) const fn is_default_char(c: u8) -> bool {
    matches!(c, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z' | b'0'..=b'9')
}

/// Is `c` allowed at 1-based position `i`, given pos-3 and the car/MPV/LT flag?
/// Mirrors the `CASE` over `patternMY` / `patternNumbersOnly` / `patternDefault`
/// in `fVINCheckDigit2`. Case-insensitive (input is upper-cased by the caller).
pub(crate) fn valid_at(i: usize, c: u8, pos3: u8, is_car_mpv_lt: bool) -> bool {
    // patternMY = [A-H,J-N,P,R-T,V-Y,1-9]; patternNumbersOnly = [0-9];
    // patternDefault = [A-H,J-N,P,R-Z,0-9].
    let my = is_my_char(c);
    let nums = c.is_ascii_digit();
    let default = is_default_char(c);
    match i {
        10 => my,
        13 if pos3 != b'9' && is_car_mpv_lt => nums,
        14 if pos3 != b'9' => nums,
        _ if i >= 15 => nums,
        _ => default,
    }
}

/// Which `fVINCheckDigit*` per-position rule the shared kernel applies. Selecting
/// the predicate through a small `Copy` discriminant (rather than a closure) keeps
/// the kernel a single concrete function: after inlining, `rule` is a constant and
/// the match const-folds, so this costs nothing over the two hand-written loops.
#[derive(Clone, Copy)]
enum PosRule {
    /// `fVINCheckDigit2`: position 13 is numeric only for a car/MPV/LT VIN.
    V2 { is_car_mpv_lt: bool },
    /// `fVINCheckDigit`: positions 13 and 14 are both numeric.
    V1,
}

// The largest valid weighted sum is 9 * 89 = 801. An invalid byte contributes
// 1024, so one final comparison detects invalid positions without a branch for
// every character. The WMI-dependent rules for positions 13/14 stay per-call.
const INVALID: u16 = 1024;
const WEIGHTED: [[u16; 256]; 17] = {
    let weights = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];
    let mut values = [[INVALID; 256]; 17];
    let mut pos = 0;
    while pos < 17 {
        let mut byte = 0;
        while byte < 256 {
            let c = byte as u8;
            let allowed = if pos == 9 {
                is_my_char(c)
            } else if pos >= 14 {
                c.is_ascii_digit()
            } else {
                is_default_char(c)
            };
            if allowed {
                if let Some(value) = translit(c) {
                    values[pos][byte] = value as u16 * weights[pos];
                }
            }
            byte += 1;
        }
        pos += 1;
    }
    values
};

/// Shared body of both `fVINCheckDigit*` ports: transliterate-and-weight over the
/// 17 positions, returning `Some('?')` for any character invalid at its
/// position or untransliteratable, `None` when the VIN is not 17 characters. The
/// only thing that differs between the ports is the per-position validity rule.
///
/// The `'?'` sentinel is load-bearing for oracle parity, not a placeholder to
/// tighten: a VIN with a literal '?' at position 9 check-digit-*validates* because
/// the caller compares it against this '?' ('?' == '?'). Matched deliberately in
/// `errors.rs::compute_errors` — see there before changing this return.
#[inline]
fn check_digit_kernel(vin: &str, pos3: u8, rule: PosRule) -> Option<char> {
    let b = vin.as_bytes();
    if b.len() != 17 {
        return None;
    }
    let numeric13 = matches!(
        rule,
        PosRule::V1
            | PosRule::V2 {
                is_car_mpv_lt: true
            }
    );
    if pos3 != b'9' && (!b[13].is_ascii_digit() || numeric13 && !b[12].is_ascii_digit()) {
        return Some('?');
    }
    let sum: u16 = b
        .iter()
        .enumerate()
        .map(|(i, &c)| WEIGHTED[i][c as usize])
        .sum();
    if sum >= INVALID {
        return Some('?');
    }
    let r = sum % 11;
    Some(if r == 10 {
        'X'
    } else {
        (b'0' + r as u8) as char
    })
}

/// Compute the position-9 check digit per `fVINCheckDigit2`. Returns `Some('X')`
/// or `Some(d)`, `Some('?')` if any character is invalid at its position, or
/// `None` when the VIN is not 17 characters (the SQL returns `''`).
pub fn check_digit_with_flag(vin: &str, is_car_mpv_lt: bool) -> Option<char> {
    // Only read once len is known good; the kernel guards the length before
    // consulting the rule, so a short VIN never indexes here.
    let pos3 = vin.as_bytes().get(2).copied().unwrap_or(0);
    check_digit_kernel(vin, pos3, PosRule::V2 { is_car_mpv_lt })
}

/// Convenience wrapper for `fVINCheckDigit2(vin, false)`.
pub fn check_digit(vin: &str) -> Option<char> {
    check_digit_with_flag(vin, false)
}

/// Per-position validity for the single-arg `fVINCheckDigit` (used by error code
/// 3). Differs from `fVINCheckDigit2`: positions 13 AND 14 are numeric whenever
/// position 3 is not `'9'` (no car/MPV/LT gating on position 13).
#[cfg(test)]
fn valid_at_v1(i: usize, c: u8, pos3: u8) -> bool {
    let my = is_my_char(c);
    let nums = c.is_ascii_digit();
    let default = is_default_char(c);
    match i {
        10 => my,
        13 | 14 if pos3 != b'9' => nums,
        _ if i >= 15 => nums,
        _ => default,
    }
}

/// Port of the single-arg `vpic.fVINCheckDigit` (canonical:
/// `vpic/procs/fvincheckdigit.sql`). Same transliteration/weights as
/// `fVINCheckDigit2`; only the position 13/14 validity classes differ.
pub fn check_digit_v1(vin: &str) -> Option<char> {
    let pos3 = vin.as_bytes().get(2).copied().unwrap_or(0);
    check_digit_kernel(vin, pos3, PosRule::V1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_kernel_matches_positional_rules_for_every_ascii_mutation() {
        fn reference(vin: &str, rule: PosRule) -> Option<char> {
            let bytes = vin.as_bytes();
            if bytes.len() != 17 {
                return None;
            }
            let weights = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];
            let mut sum = 0;
            for (i, &byte) in bytes.iter().enumerate() {
                let valid = match rule {
                    PosRule::V1 => valid_at_v1(i + 1, byte, bytes[2]),
                    PosRule::V2 { is_car_mpv_lt } => valid_at(i + 1, byte, bytes[2], is_car_mpv_lt),
                };
                if !valid {
                    return Some('?');
                }
                let Some(value) = translit(byte) else {
                    return Some('?');
                };
                sum += weights[i] * value;
            }
            Some(if sum % 11 == 10 {
                'X'
            } else {
                (b'0' + (sum % 11) as u8) as char
            })
        }

        for low_volume in [false, true] {
            let mut base = *b"1HGCM82633A004352";
            if low_volume {
                base[2] = b'9';
            }
            for rule in [
                PosRule::V1,
                PosRule::V2 {
                    is_car_mpv_lt: false,
                },
                PosRule::V2 {
                    is_car_mpv_lt: true,
                },
            ] {
                for pos in 0..17 {
                    for byte in 0..=127 {
                        let mut bytes = base;
                        bytes[pos] = byte;
                        let vin = std::str::from_utf8(&bytes).unwrap();
                        assert_eq!(
                            check_digit_kernel(vin, bytes[2], rule),
                            reference(vin, rule),
                            "{vin:?}"
                        );
                    }
                }
                for vin in [
                    "",
                    "1HGCM82633A00435",
                    "1HGCM82633A004352X",
                    "éGCM82633A004352",
                    "AAAAAAAAAAAAAAAAA",
                ] {
                    assert_eq!(
                        check_digit_kernel(vin, vin.as_bytes().get(2).copied().unwrap_or(0), rule),
                        reference(vin, rule),
                        "{vin:?}"
                    );
                }
            }
        }
    }

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
