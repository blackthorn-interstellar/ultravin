//! Cold-start probe: a fresh process that loads the embedded artifact and runs
//! one decode, printing the elapsed milliseconds from `main` entry to the first
//! decode being complete. The parent harness also wraps this in an external
//! wall-clock timer (process spawn + exit) for an end-to-end number.

use std::time::Instant;

fn main() {
    let t0 = Instant::now();
    let vin = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1HGCM82633A004352".to_string());
    let r = ultravin_core::decode(&vin);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    // Touch the result so the load+decode can't be optimized away.
    eprintln!("cold_ms={ms:.3} elements={}", r.elements.len());
    println!("{ms:.3}");
}
