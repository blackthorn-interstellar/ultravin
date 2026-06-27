//! Criterion benches for the decode hot path.
//!
//! - `warm_single`: steady-state cost of one decode (db already loaded).
//! - `batch`: single-core throughput over the frozen-corpus VIN list.
//!
//! Cold-start (process spawn + artifact load + first decode) is measured out of
//! band by the parent harness — criterion can only see warm, in-process cost.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::hint::black_box;
use ultravin_core::{decode, decode_with, Db};

/// The frozen-corpus VINs (valid 17-char), one per line.
static VINS: &str = include_str!("vins.txt");

fn vins() -> Vec<&'static str> {
    VINS.lines().filter(|l| l.len() == 17).collect()
}

/// A single canonical, fully-decoding VIN for the warm single-decode number.
const WARM_VIN: &str = "1HGCM82633A004352";

fn bench_warm_single(c: &mut Criterion) {
    // Inject a fixed clock so the number is reproducible run-to-run.
    let db = Db::embedded();
    c.bench_function("warm_single", |b| {
        b.iter(|| decode_with(db, black_box(WARM_VIN), 1_750_000_000_000_000, 2026));
    });
}

fn bench_batch(c: &mut Criterion) {
    let db = Db::embedded();
    let vins = vins();
    let mut g = c.benchmark_group("batch");
    g.throughput(Throughput::Elements(vins.len() as u64));
    g.bench_function("corpus", |b| {
        b.iter(|| {
            for v in &vins {
                black_box(decode_with(db, black_box(v), 1_750_000_000_000_000, 2026));
            }
        });
    });
    g.finish();
}

/// Touch `decode` (system-clock path) once so it stays linked/benchable.
fn bench_warm_syscall(c: &mut Criterion) {
    c.bench_function("warm_single_sysclock", |b| {
        b.iter(|| decode(black_box(WARM_VIN)));
    });
}

criterion_group!(benches, bench_warm_single, bench_batch, bench_warm_syscall);
criterion_main!(benches);
