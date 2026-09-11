//! Fingerprint the entire decode result at a fixed clock, including provenance.
//!
//! Input is JSONL `[vin, caller_year]`; output is one BLAKE3 hash per input.
//! Save this executable before an optimization and compare its output with the
//! rebuilt executable on the same inputs. No oracle normalization hides changes.
//! With `json`, also assert the public JSON bytes equal serde's full result for
//! every input. That mode uses the system clock and retries across second changes
//! so both paths are compared at the same instant; the default mode stays fixed.

use std::io::{self, BufRead, Write};

fn main() {
    let db = ultravin::Db::embedded();
    let verify_json = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("json") => true,
        _ => panic!("optional mode must be json"),
    };
    let mut out = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let (vin, year): (String, Option<i32>) =
            serde_json::from_str(&line.expect("read input")).expect("[vin, caller_year]");
        let json = if verify_json {
            loop {
                let now = ultravin::now_micros();
                let actual = ultravin::decode_json(&vin, year);
                let expected =
                    ultravin::decode_full(db, &vin, now, ultravin::current_year_at(now), year);
                if ultravin::now_micros() == now {
                    let expected = serde_json::to_vec(&expected).expect("serialize decode");
                    assert_eq!(
                        actual.as_bytes(),
                        expected,
                        "JSON mismatch for {vin:?}, {year:?}"
                    );
                    break actual.into_bytes();
                }
            }
        } else {
            let result = ultravin::decode_full(db, &vin, 1_788_739_200_000_000, 2026, year);
            serde_json::to_vec(&result).expect("serialize decode")
        };
        writeln!(out, "{}", blake3::hash(&json)).expect("write fingerprint");
    }
}
