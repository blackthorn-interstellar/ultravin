//! Decode every VIN in a file — the code-coverage probe for corpus minimization.
//!
//! Measures which regions of the decode engine a VIN corpus actually reaches:
//! `cargo llvm-cov run --example covrun --summary-only -- vins.txt`.
//! Compare a candidate corpus against an exhaustive one; equal region coverage
//! means the small corpus exercises the same code.
//!
//! Every public entry point is driven, not just `decode` — the batch, flat and
//! JSON wrappers are a third of `lib.rs`, and a corpus cannot be blamed for
//! regions the probe never calls.

use std::{env, fs};

fn main() {
    let path = env::args().nth(1).expect("usage: covrun <vin-file>");
    let text = fs::read_to_string(&path).expect("read vin file");
    let vins: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let mut elements = 0usize;
    let mut bytes = 0usize;
    for vin in &vins {
        elements += ultravin::decode(vin, None).elements.len();
        bytes +=
            ultravin::decode_json(vin, None).len() + ultravin::decode_json_flat(vin, None).len();
    }
    elements += ultravin::decode_batch(&vins, None).len();
    elements += ultravin::decode_batch_flat(&vins, None).len();
    bytes += ultravin::decode_batch_json(&vins, None).len();
    bytes += ultravin::decode_batch_json_flat(&vins, None).len();

    eprintln!(
        "covrun: {} VINs, {elements} elements, {bytes} json bytes",
        vins.len()
    );
}
