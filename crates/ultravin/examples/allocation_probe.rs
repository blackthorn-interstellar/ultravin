//! Single-thread allocation diagnostic for full native decoding.
//!
//! Usage: `allocation_probe [corpus-path] [rows]`. The selected unique VINs are
//! warmed once, then decoded once with a frozen clock while allocator activity
//! on this thread is counted.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::collections::HashSet;

const DEFAULT_NOW_MICROS: i64 = 1_788_220_800_000_000;
const HISTOGRAM_BINS: usize = usize::BITS as usize + 1;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static REALLOCS: Cell<u64> = const { Cell::new(0) };
    static FREES: Cell<u64> = const { Cell::new(0) };
    static ALLOCATED_BYTES: Cell<u64> = const { Cell::new(0) };
    static REALLOC_OLD_SIZE: Cell<[u64; HISTOGRAM_BINS]> = const { Cell::new([0; HISTOGRAM_BINS]) };
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
type InnerAllocator = mimalloc::MiMalloc;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
type InnerAllocator = std::alloc::System;

struct CountingAllocator(InnerAllocator);

fn counting_enabled() -> bool {
    ENABLED.try_with(Cell::get).unwrap_or(false)
}

fn increment(counter: &'static std::thread::LocalKey<Cell<u64>>, amount: u64) {
    let _ = counter.try_with(|value| value.set(value.get().wrapping_add(amount)));
}

// The wrapper delegates allocation unchanged to mimalloc on the wheel's target
// architectures and to System elsewhere. Its instrumentation touches only
// const-initialized thread-local Cells, so callbacks do not allocate, lock, or
// share mutable state with another thread. `try_with` also makes TLS teardown a
// counter-disabled path rather than a panic from inside GlobalAlloc.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting_enabled() {
            increment(&ALLOCS, 1);
            increment(&ALLOCATED_BYTES, layout.size() as u64);
        }
        // SAFETY: the caller supplies the GlobalAlloc layout contract unchanged.
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if counting_enabled() {
            increment(&FREES, 1);
        }
        // SAFETY: the pointer and layout are forwarded to their original allocator.
        unsafe { self.0.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counting_enabled() {
            increment(&REALLOCS, 1);
            increment(&ALLOCATED_BYTES, new_size as u64);
            let _ = REALLOC_OLD_SIZE.try_with(|histogram| {
                let mut bins = histogram.get();
                let bin = if layout.size() == 0 {
                    0
                } else {
                    layout.size().next_power_of_two().trailing_zeros() as usize + 1
                };
                bins[bin] = bins[bin].wrapping_add(1);
                histogram.set(bins);
            });
        }
        // SAFETY: the allocation and its original layout are forwarded unchanged.
        unsafe { self.0.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
static GLOBAL: CountingAllocator = CountingAllocator(mimalloc::MiMalloc);
#[global_allocator]
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
static GLOBAL: CountingAllocator = CountingAllocator(std::alloc::System);

fn reset_and_enable() {
    ALLOCS.set(0);
    REALLOCS.set(0);
    FREES.set(0);
    ALLOCATED_BYTES.set(0);
    REALLOC_OLD_SIZE.set([0; HISTOGRAM_BINS]);
    ENABLED.set(true);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| "scripts/bench/corpus.txt".to_string());
    let row_limit = args
        .next()
        .map(|value| value.parse::<usize>().expect("rows must be an integer"))
        .unwrap_or(3_000);
    let now_micros = std::env::var("ULTRAVIN_NOW_MICROS")
        .ok()
        .map(|value| {
            value
                .parse::<i64>()
                .expect("ULTRAVIN_NOW_MICROS must be an integer")
        })
        .unwrap_or(DEFAULT_NOW_MICROS);

    let corpus = std::fs::read_to_string(&path).expect("read corpus");
    let mut seen = HashSet::new();
    let vins: Vec<&str> = corpus
        .lines()
        .filter(|vin| vin.len() == 17 && seen.insert(*vin))
        .take(row_limit)
        .collect();
    assert!(!vins.is_empty(), "empty corpus: {path}");

    for vin in &vins {
        drop(std::hint::black_box(ultravin::decode_at(
            vin, None, now_micros,
        )));
    }

    reset_and_enable();
    for vin in &vins {
        drop(std::hint::black_box(ultravin::decode_at(
            vin, None, now_micros,
        )));
    }
    ENABLED.set(false);

    let histogram: Vec<_> = REALLOC_OLD_SIZE
        .get()
        .into_iter()
        .enumerate()
        .filter(|&(_, count)| count > 0)
        .map(|(bin, count)| {
            let upper_bound = if bin == 0 { 0 } else { 1_usize << (bin - 1) };
            serde_json::json!({"old_size_power2_ceiling": upper_bound, "count": count})
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "kind": "ultravin_allocation_probe",
            "rows": vins.len(),
            "now_micros": now_micros,
            "allocations": ALLOCS.get(),
            "reallocations": REALLOCS.get(),
            "frees": FREES.get(),
            "allocated_bytes": ALLOCATED_BYTES.get(),
            "realloc_old_size_histogram": histogram,
        })
    );
}
