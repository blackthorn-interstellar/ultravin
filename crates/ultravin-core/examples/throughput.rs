//! 60-second throughput probe for the in-process engine.
//!
//! Decodes the shared benchmark corpus (scripts/bench/corpus.txt, passed as
//! argv[1]) on repeat until the wall-clock budget is spent, reporting VIN/s for
//! the single-stream path and the parallel `decode_batch` path. This is the
//! engine ceiling the SQL oracles in scripts/bench/throughput.py are measured
//! against. Run:
//! `cargo run -p ultravin-core --example throughput --release -- scripts/bench/corpus.txt [secs]`

use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| "scripts/bench/corpus.txt".into());
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let budget = Duration::from_secs(secs);

    let corpus = std::fs::read_to_string(&path).expect("read corpus");
    let vins: Vec<&str> = corpus.lines().filter(|l| l.len() == 17).collect();
    assert!(!vins.is_empty(), "empty corpus: {path}");

    // Single-stream: one sequential caller, system-clock path (what a caller sees).
    let _ = ultravin_core::decode(vins[0]); // warm caches
    let t = Instant::now();
    let mut n: u64 = 0;
    while t.elapsed() < budget {
        // Decode a full pass so we never check the clock more than per-corpus.
        for v in &vins {
            std::hint::black_box(ultravin_core::decode(v));
        }
        n += vins.len() as u64;
    }
    let dt = t.elapsed().as_secs_f64();
    report("single", n, dt, 1);

    // Batched: rayon over the shared archive across all cores.
    let owned: Vec<String> = vins.iter().map(|s| s.to_string()).collect();
    let _ = ultravin_core::decode_batch(&owned); // warm per-thread caches
    let t = Instant::now();
    let mut n: u64 = 0;
    while t.elapsed() < budget {
        std::hint::black_box(ultravin_core::decode_batch(&owned));
        n += owned.len() as u64;
    }
    let dt = t.elapsed().as_secs_f64();
    report("batch", n, dt, rayon::current_num_threads());
}

fn report(label: &str, n: u64, dt: f64, cores: usize) {
    let per_s = n as f64 / dt;
    eprintln!(
        "{label}: {n} VINs in {dt:.1}s = {per_s:.0} VIN/s ({:.0} in 60s, {cores} core(s))",
        per_s * 60.0,
    );
}
