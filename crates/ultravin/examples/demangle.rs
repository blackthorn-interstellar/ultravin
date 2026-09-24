//! Demangle Rust symbols, one per stdin line, without crate hashes.
//!
//! `scripts/coverage.py` keys its allowances on these names. The raw llvm-cov
//! names embed crate disambiguators that change with every dependency bump.

use std::io::{self, BufRead, Write};

fn main() {
    let mut out = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let line = line.expect("read stdin");
        writeln!(out, "{:#}", rustc_demangle::demangle(line.trim())).expect("write stdout");
    }
}
