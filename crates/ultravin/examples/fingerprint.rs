//! Fingerprint the entire decode result at a fixed clock, including provenance.
//!
//! Input is JSONL `[vin, caller_year]`; output is one BLAKE3 hash per input.
//! Save this executable before an optimization and compare its output with the
//! rebuilt executable on the same inputs. No oracle normalization hides changes.

use std::io::{self, BufRead, Write};

fn main() {
    let db = ultravin::Db::embedded();
    let mut out = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let (vin, year): (String, Option<i32>) =
            serde_json::from_str(&line.expect("read input")).expect("[vin, caller_year]");
        let result = ultravin::decode_full(db, &vin, 1_788_739_200_000_000, 2026, year);
        let json = serde_json::to_vec(&result).expect("serialize decode");
        writeln!(out, "{}", blake3::hash(&json)).expect("write fingerprint");
    }
}
