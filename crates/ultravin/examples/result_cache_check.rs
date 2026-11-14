//! Check every native-stream result against an uncached `decode_full` at the
//! same fixed clock, so the shared result cache can serve nothing else:
//! `result_cache_check <vins.txt> [workers] [limit] [caller-year]`.
//! `ULTRAVIN_RESULT_CACHE` sets the entries of the stream's table.

use rayon::prelude::*;

const NOW: i64 = 1_788_220_800_000_000;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("path to one VIN per line");
    let workers: usize = args.next().map_or(1, |s| s.parse().expect("workers"));
    let limit: usize = args
        .next()
        .map_or(usize::MAX, |s| s.parse().expect("limit"));
    let caller_year: Option<i32> = args.next().map(|s| s.parse().expect("caller year"));
    let text = std::fs::read_to_string(path).expect("read input");
    let inputs: Vec<String> = text.lines().take(limit).map(str::to_owned).collect();
    drop(text);
    let db = ultravin::Db::embedded();
    let year = ultravin::current_year_at(NOW);
    let config = ultravin::NativeStreamConfig {
        workers,
        batch_size: 64,
        slots_per_worker: 4,
        max_inflight_rows: workers * 64 * 4,
    };
    let (hits_before, probes_before, uncopied_before) = ultravin::result_cache_counters();
    let mut pending = Vec::new();
    let mut mismatches = 0_usize;
    let mut verify = |pending: &mut Vec<(usize, ultravin::DecodeResult<'static>)>| {
        let bad: Vec<usize> = pending
            .par_iter()
            .filter(|(index, actual)| {
                *actual != ultravin::decode_full(db, &inputs[*index], NOW, year, caller_year)
            })
            .map(|(index, _)| *index)
            .collect();
        for index in &bad {
            eprintln!("mismatch at line {}: {:?}", index + 1, inputs[*index]);
        }
        mismatches += bad.len();
        pending.clear();
    };
    db.decode_native_stream_at(&inputs, caller_year, NOW, config, |batch| {
        let start = batch.start_index();
        pending.extend(
            batch
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, r)| (start + i, r)),
        );
        if pending.len() >= 1 << 15 {
            verify(&mut pending);
        }
    })
    .expect("native stream");
    verify(&mut pending);
    let (hits, probes, uncopied) = ultravin::result_cache_counters();
    println!(
        "checked {} rows, {mismatches} mismatches, {} hits ({} uncopied) / {} probes",
        inputs.len(),
        hits - hits_before,
        uncopied - uncopied_before,
        probes - probes_before
    );
    assert_eq!(mismatches, 0);
}
