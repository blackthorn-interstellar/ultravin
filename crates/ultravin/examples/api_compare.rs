//! One public Rust API, timed with its output consumed, for
//! `scripts/bench/api_compare.py`. Run:
//! `api_compare CELL INPUT SECS [OUT]` with `ULTRAVIN_NOW_MICROS` set.
//!
//! INPUT is one VIN per line, except for `parquet` (a Parquet file with a
//! `vin` column, decoded to OUT). One untimed warm-up pass, then whole passes
//! until SECS have elapsed (at least one). Every pass must return the warm-up's
//! checksum, so a cell is deterministic or it fails. Prints one JSON line.
//!
//! The `*-read` cells fold every header field and every element field of every
//! result into a position-weighted sum: integers and flags by value, strings by
//! length and the sum of their bytes. Printing it keeps the optimiser from
//! skipping a read.
//! `native-read` and `batch-full-read` see the same results, so their checksums
//! must agree.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use arrow_array::{RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use ultravin::{DecodeResult, FlatResult, FlatValue};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Rows per call for the batch APIs: the size a caller would hand them.
const CALL_ROWS: usize = 200_000;
/// Rows per `RecordBatch`: the dataset door's default chunk.
const ARROW_ROWS: usize = 65_536;

/// A string's length and the sum of its bytes: reads every byte.
fn text(s: &str) -> u64 {
    s.bytes()
        .fold(s.len() as u64 * 257, |a, b| a + u64::from(b))
}

fn int(v: Option<impl Into<i64>>) -> u64 {
    v.map_or(u64::MAX, |v| v.into() as u64)
}

/// Weight each term by its position, so a swapped field or element changes the
/// sum. The terms are independent (no serial hash chain), so the fold costs
/// about what reading the fields costs.
fn weigh(terms: impl IntoIterator<Item = u64>) -> u64 {
    terms
        .into_iter()
        .zip(1_u64..)
        .fold(0, |s, (t, k)| s.wrapping_add(t.wrapping_mul(2 * k + 1)))
}

fn header(
    vin: &str,
    wmi: &str,
    descriptor: &str,
    year: Option<i32>,
    codes: &[i32],
    valid: bool,
    corrected: &str,
) -> u64 {
    let codes = weigh(codes.iter().map(|&c| c as u64));
    weigh([
        text(vin),
        text(wmi),
        text(descriptor),
        int(year),
        codes,
        u64::from(valid),
        text(corrected),
    ])
}

fn read_full(r: &DecodeResult) -> u64 {
    let elements = weigh(r.elements.iter().map(|e| {
        weigh([
            text(e.group_name),
            text(e.variable),
            text(&e.value),
            e.element_id as u64,
            text(&e.attribute_id),
            text(e.code),
            text(e.data_type),
            text(e.decode),
            text(&e.source),
            int(e.pattern_id),
            int(e.vin_schema_id),
            text(&e.keys),
            int(e.created_on),
            int(e.wmi_id),
            u64::from(e.to_be_qced),
        ])
    }));
    let h = header(
        &r.vin,
        &r.wmi,
        &r.descriptor,
        r.model_year,
        &r.error_codes,
        r.check_digit_valid,
        &r.corrected_vin,
    );
    weigh([h, elements])
}

fn read_flat(r: &FlatResult) -> u64 {
    let attributes = weigh(r.attributes.iter().map(|(key, value)| {
        let value = match value {
            FlatValue::One(v) => text(v),
            FlatValue::Many(vs) => weigh(vs.iter().map(|v| text(v))),
        };
        weigh([text(key), value])
    }));
    let h = header(
        &r.vin,
        &r.wmi,
        &r.descriptor,
        r.model_year,
        &r.error_codes,
        r.check_digit_valid,
        &r.corrected_vin,
    );
    weigh([h, attributes])
}

/// Rows, checksum and output bytes of one whole pass.
type Pass = (u64, u64, u64);

fn native(vins: &[String], now: i64, read: bool) -> Pass {
    let (mut rows, mut sum) = (0_u64, 0_u64);
    let options = ultravin::NativeAutoOptions::default();
    ultravin::decode_native_stream_auto_at(vins, None, now, options, |batch| {
        rows += batch.len() as u64;
        if read {
            sum = batch.iter().fold(sum, |s, r| s.wrapping_add(read_full(r)));
        } else {
            std::hint::black_box(batch);
        }
    })
    .expect("native stream");
    (rows, sum, 0)
}

fn chunked<T>(
    vins: &[String],
    call: impl Fn(&[String]) -> Vec<T>,
    read: impl Fn(&T) -> u64,
) -> Pass {
    let (mut rows, mut sum) = (0_u64, 0_u64);
    for chunk in vins.chunks(CALL_ROWS) {
        let results = call(chunk);
        rows += results.len() as u64;
        sum = results.iter().fold(sum, |s, r| s.wrapping_add(read(r)));
    }
    (rows, sum, 0)
}

fn json(vins: &[String], call: impl Fn(&[String]) -> String) -> Pass {
    let bytes = vins.chunks(CALL_ROWS).map(|c| call(c).len() as u64).sum();
    (vins.len() as u64, bytes, bytes)
}

fn arrow(batches: &[RecordBatch], decoder: &ultravin::ArrowDecoder) -> Pass {
    let (mut rows, mut sum, mut bytes) = (0_u64, 0_u64, 0_u64);
    for batch in batches {
        let out = decoder.decode_batch(batch).expect("arrow decode");
        rows += out.num_rows() as u64;
        for column in out.columns() {
            sum = sum.wrapping_add(weigh([column.null_count() as u64, column.len() as u64]));
            bytes += column.get_array_memory_size() as u64;
        }
    }
    (rows, weigh([sum, rows]), bytes)
}

fn all_columns() -> Vec<ultravin::ColumnSpec> {
    let ids = ultravin::all_public_ids(ultravin::Db::embedded());
    ids.into_iter().map(ultravin::ColumnSpec::from).collect()
}

fn parquet(src: &Path, dst: &Path) -> Pass {
    let opts = ultravin::parquet_io::ParquetOpts {
        columns: all_columns(),
        ..Default::default()
    };
    let rows = ultravin::parquet_io::decode_parquet_to_file(src, dst, opts).expect("parquet");
    let bytes = std::fs::metadata(dst).expect("parquet output").len();
    (rows as u64, weigh([rows as u64, bytes]), bytes)
}

fn lines(path: &str) -> Vec<String> {
    let corpus = std::fs::read_to_string(path).expect("read corpus");
    corpus.lines().map(str::to_owned).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [cell, input, secs, rest @ ..] = args.as_slice() else {
        panic!("usage: api_compare CELL INPUT SECS [OUT]");
    };
    let secs: f64 = secs.parse().expect("SECS must be a number");
    let now: i64 = std::env::var("ULTRAVIN_NOW_MICROS")
        .expect("set ULTRAVIN_NOW_MICROS")
        .parse()
        .expect("ULTRAVIN_NOW_MICROS must be an integer");

    let vins = if cell == "parquet" {
        Vec::new()
    } else {
        lines(input)
    };
    let schema = Arc::new(Schema::new(vec![Field::new("vin", DataType::Utf8, true)]));
    let (batches, decoder) = if cell == "arrow" {
        let batches = vins
            .chunks(ARROW_ROWS)
            .map(|c| {
                let column = Arc::new(StringArray::from_iter_values(c));
                RecordBatch::try_new(schema.clone(), vec![column]).expect("input batch")
            })
            .collect();
        let opts = ultravin::ArrowOpts {
            columns: all_columns(),
            ..Default::default()
        };
        (
            batches,
            Some(ultravin::ArrowDecoder::new_at(&schema, &opts, now).expect("decoder")),
        )
    } else {
        (Vec::new(), None)
    };
    let out = rest.first().map(Path::new);
    let pass = || -> Pass {
        match cell.as_str() {
            "native-discard" => native(&vins, now, false),
            "native-read" => native(&vins, now, true),
            "batch-full-read" => chunked(
                &vins,
                |c| ultravin::decode_batch_at(c, None, now),
                read_full,
            ),
            "batch-flat-read" => chunked(
                &vins,
                |c| ultravin::decode_batch_flat_at(c, None, now),
                read_flat,
            ),
            "json-full" => json(&vins, |c| ultravin::decode_batch_json_at(c, None, now)),
            "json-flat" => json(&vins, |c| ultravin::decode_batch_json_flat_at(c, None, now)),
            "jsonl-flat" => json(&vins, |c| {
                ultravin::decode_batch_jsonl_at(c, None, now, false)
            }),
            "arrow" => arrow(&batches, decoder.as_ref().expect("arrow decoder")),
            "parquet" => parquet(Path::new(input), out.expect("parquet needs OUT")),
            other => panic!("unknown cell {other}"),
        }
    };

    let (_, warm_sum, _) = pass();
    let started = Instant::now();
    let (mut rows, mut passes, mut bytes) = (0_u64, 0_u64, 0_u64);
    while passes == 0 || started.elapsed().as_secs_f64() < secs {
        let (n, sum, b) = pass();
        assert_eq!(sum, warm_sum, "pass {passes} checksum differs from warm-up");
        rows += n;
        bytes += b;
        passes += 1;
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({
            "cell": cell,
            "rows": rows,
            "passes": passes,
            "timed_seconds": elapsed,
            "rows_per_second": rows as f64 / elapsed,
            "workers": ultravin::predictor::worker_count(),
            "checksum": format!("{warm_sum:016x}"),
            "output_bytes_per_pass": bytes / passes,
        })
    );
}
