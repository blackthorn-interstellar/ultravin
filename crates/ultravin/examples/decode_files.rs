//! Disk-to-disk decode timings: the compact pair against one wide parquet.
//!
//! `cargo run -p ultravin --release --features parquet --example decode_files -- MODE SRC DST`
//!
//! - `compact SRC DST_DIR`: [`ultravin::parquet_io::decode_compact_at`].
//! - `wide SRC DST`: what `decode_parquet_to_file` does, at the same instant.
//! - `parquet SRC.txt DST`: one VIN per line to a one-column (`vin`) parquet,
//!   every third row also given a `year` when a fourth argument says `years`.
//!
//! Every mode projects all public elements. `ULTRAVIN_NOW_MICROS` fixes the
//! clock; `RAYON_NUM_THREADS` the workers.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use ultravin::parquet_io::{self, ParquetOpts};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, src, dst, rest @ ..] = args.as_slice() else {
        panic!("usage: decode_files compact|wide|parquet SRC DST [years]");
    };
    let (src, dst) = (Path::new(src), Path::new(dst));
    let now = std::env::var("ULTRAVIN_NOW_MICROS")
        .ok()
        .map_or_else(ultravin::now_micros, |value| {
            value.parse().expect("ULTRAVIN_NOW_MICROS is an integer")
        });
    let opts = ParquetOpts {
        columns: ultravin::all_public_ids(ultravin::Db::embedded())
            .into_iter()
            .map(ultravin::ColumnSpec::Id)
            .collect(),
        ..ParquetOpts::default()
    };
    let started = Instant::now();
    let rows = match mode.as_str() {
        "compact" => {
            let rows = parquet_io::decode_compact_at(src, dst, opts, now).expect("compact decode");
            eprintln!("vehicles: {}", rows.vehicles);
            rows.vins
        }
        "wide" => {
            let iter = parquet_io::open_chunks_at(src, opts, now).expect("open source");
            let schema = iter.out_schema.clone().expect("a schema");
            parquet_io::write_parquet(iter, schema, dst, 65_536).expect("wide decode")
        }
        "parquet" => to_parquet(src, dst, rest.first().is_some_and(|arg| arg == "years")),
        other => panic!("unknown mode {other:?}"),
    };
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "{{\"mode\":\"{mode}\",\"rows\":{rows},\"seconds\":{seconds:.3},\"vins_per_second\":{:.0}}}",
        rows as f64 / seconds
    );
}

fn to_parquet(src: &Path, dst: &Path, years: bool) -> usize {
    use arrow_array::{ArrayRef, Int32Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};

    let text = std::fs::read_to_string(src).expect("read text corpus");
    let vins: Vec<&str> = text.lines().collect();
    let mut fields = vec![Field::new("vin", DataType::Utf8, true)];
    let mut columns: Vec<ArrayRef> = vec![Arc::new(StringArray::from(vins.clone()))];
    if years {
        fields.push(Field::new("year", DataType::Int32, true));
        let year = |row: usize| row.is_multiple_of(3).then_some(2000 + (row % 25) as i32);
        columns.push(Arc::new((0..vins.len()).map(year).collect::<Int32Array>()));
    }
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).expect("batch");
    let schema = batch.schema();
    parquet_io::write_parquet(std::iter::once(Ok(batch)), schema, dst, 1 << 20)
        .expect("write parquet")
}
