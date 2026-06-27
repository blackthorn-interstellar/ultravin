//! ultravin-core — pure-Rust NHTSA vPIC VIN decoder engine.
//!
//! Goal: byte-for-byte parity with the official Postgres `vpic.spvindecode`
//! (see `vpic/procs/*.sql`, the canonical spec). This crate currently ships the
//! *data-free* steps that need no embedded artifact — VIN normalization, the
//! WMI/descriptor extraction (`fVinWMI`/`fVinDescriptor`) and the check digit
//! (`fVINCheckDigit2`). The data-dependent pattern/schema/element resolution is
//! built in workflow 1 against the embedded artifact behind the `Db` seam.

mod checkdigit;
mod wmi;

pub use checkdigit::check_digit;
pub use wmi::{vin_descriptor, vin_wmi};

/// A single decoded VIN result. The shape grows to the full 15-column
/// `spvindecode` contract once the data engine lands; this is the data-free core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeResult {
    /// Normalized (upper-cased, trimmed) input VIN.
    pub vin: String,
    /// World Manufacturer Identifier (3 chars, or 6 when position 3 is `9`).
    pub wmi: String,
    /// Vehicle descriptor (`fVinDescriptor`) — pos 1-11 (or 1-14 for low-volume).
    pub descriptor: String,
    /// `true` when the position-9 check digit matches `fVINCheckDigit2`.
    pub check_digit_valid: bool,
    /// Space-delimited error codes vPIC would emit, as `(code, text)` pairs.
    pub errors: Vec<(u16, String)>,
}

/// VIN characters allowed by vPIC: digits and A-Z excluding I, O, Q.
fn is_valid_vin_char(c: u8) -> bool {
    matches!(c, b'0'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z')
}

/// Decode a VIN through the data-free steps. Full decode (patterns, schemas,
/// elements) requires the embedded artifact and arrives in workflow 1.
pub fn decode(input: &str) -> DecodeResult {
    let vin = input.trim().to_ascii_uppercase();
    let mut errors: Vec<(u16, String)> = Vec::new();

    if vin.len() < 17 {
        errors.push((6, "VIN is shorter than 17 characters".to_string()));
    }
    if vin.bytes().any(|b| !is_valid_vin_char(b)) {
        errors.push((400, "VIN contains invalid characters".to_string()));
    }

    let wmi = vin_wmi(&vin);
    let descriptor = vin_descriptor(&vin);

    let check_digit_valid = match check_digit(&vin) {
        Some(calc) if vin.len() == 17 => vin.as_bytes()[8] == calc as u8,
        _ => false,
    };
    if vin.len() == 17 && !check_digit_valid {
        errors.push((1, "Check digit does not match".to_string()));
    }

    DecodeResult {
        vin,
        wmi,
        descriptor,
        check_digit_valid,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_honda_vin_decodes_cleanly() {
        // 1HGCM82633A004352 is the canonical valid sample VIN (check digit 3).
        let r = decode("1HGCM82633A004352");
        assert_eq!(r.wmi, "1HG");
        assert!(r.check_digit_valid);
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn bad_check_digit_flagged() {
        let r = decode("1HGCM82633A004353");
        assert!(!r.check_digit_valid);
        assert!(r.errors.iter().any(|(c, _)| *c == 1));
    }

    #[test]
    fn lowercase_and_whitespace_normalized() {
        let r = decode("  1hgcm82633a004352 ");
        assert_eq!(r.vin, "1HGCM82633A004352");
        assert!(r.check_digit_valid);
    }
}
