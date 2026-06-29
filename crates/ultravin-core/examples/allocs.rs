//! Allocation counter for one decode. Wraps the system allocator with atomic
//! counters and reports allocations + bytes per decode for a few representative
//! VINs. Run: `cargo run -p ultravin-core --example allocs --release -- <vin>...`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

struct Counting;
static N: AtomicU64 = AtomicU64::new(0);
static B: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        N.fetch_add(1, Relaxed);
        B.fetch_add(l.size() as u64, Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn main() {
    let vins: Vec<String> = std::env::args().skip(1).collect();
    let vins = if vins.is_empty() {
        vec![
            "1HGCM82633A004352".to_string(), // canonical Honda
            "1FTFW1ET5DFC10312".to_string(), // Ford, code 1
            "5UXWX7C5XBA123456".to_string(), // BMW
            "JH4KA8260MC000000".to_string(), // Acura, inconclusive year -> 2 passes
        ]
    } else {
        vins
    };
    // Warm caches first (charset/matcher thread-locals + embedded db).
    for v in &vins {
        std::hint::black_box(ultravin_core::decode(v));
    }
    for v in &vins {
        let reps = 200u64;
        let n0 = N.load(Relaxed);
        let b0 = B.load(Relaxed);
        for _ in 0..reps {
            std::hint::black_box(ultravin_core::decode(v));
        }
        let dn = N.load(Relaxed) - n0;
        let db = B.load(Relaxed) - b0;
        println!(
            "{v}: {:.1} allocs/decode, {:.0} bytes/decode",
            dn as f64 / reps as f64,
            db as f64 / reps as f64
        );
    }
}
