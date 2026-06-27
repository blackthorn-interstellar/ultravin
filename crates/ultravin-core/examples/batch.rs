//! Throughput probe for the parallel `decode_batch` path.
//!
//! Replicates the frozen-corpus VINs to a large batch and times
//! `ultravin_core::decode_batch` (rayon over the shared archive). Reports
//! multi-core VIN/s. Run:
//! `cargo run -p ultravin-core --example batch --release`.

use std::time::Instant;

static VINS: &str = include_str!("../benches/vins.txt");

fn main() {
    let base: Vec<String> = VINS
        .lines()
        .filter(|l| l.len() == 17)
        .map(|s| s.to_string())
        .collect();
    let reps = 300;
    let mut batch: Vec<String> = Vec::with_capacity(base.len() * reps);
    for _ in 0..reps {
        batch.extend(base.iter().cloned());
    }
    let n = batch.len();

    // Warm the per-thread regex caches across the rayon pool.
    let _ = ultravin_core::decode_batch(&batch);

    let t = Instant::now();
    let out = ultravin_core::decode_batch(&batch);
    let dt = t.elapsed();
    assert_eq!(out.len(), n);

    let per_sec = n as f64 / dt.as_secs_f64();
    eprintln!(
        "decode_batch: {n} VINs in {:.3} ms -> {:.0} VIN/s ({} cores)",
        dt.as_secs_f64() * 1000.0,
        per_sec,
        rayon::current_num_threads(),
    );
}
