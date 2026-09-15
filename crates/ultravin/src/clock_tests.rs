//! Exercise automatic clock capture while the clock jumps between readings.

use std::cell::Cell;

use crate::*;

#[derive(Clone, Copy)]
struct Clock {
    reads: u32,
}

thread_local! {
    static CLOCK: Cell<Option<Clock>> = const { Cell::new(None) };
}

pub(super) fn read_clock() -> Option<i64> {
    CLOCK.with(|clock| {
        let mut state = clock.get()?;
        let secs = if state.reads == 0 { 0 } else { 1_767_225_600 };
        state.reads += 1;
        clock.set(Some(state));
        Some(secs)
    })
}

fn with_jumping_clock(f: impl FnOnce()) {
    struct Restore(Option<Clock>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CLOCK.with(|clock| clock.set(self.0));
        }
    }
    let _restore = Restore(CLOCK.with(|clock| clock.replace(Some(Clock { reads: 0 }))));
    f();
    CLOCK.with(|clock| {
        assert_eq!(
            clock.get().unwrap().reads,
            1,
            "a job must read its clock once"
        )
    });
}

const VIN: &str = "1HGCM82633A004352";

#[test]
fn automatic_batches_capture_one_clock_for_every_row_and_output_shape() {
    let Some(db) = Db::try_embedded() else { return };
    let inputs = vec![VIN.to_owned(); 4_103];
    let years: Vec<_> = (0..inputs.len())
        .map(|i| [Some(1995), None, Some(2003)][i % 3])
        .collect();
    let expected = decode_batch_at(&inputs, Some(&years), 0);
    assert_ne!(
        expected,
        decode_batch_at(&inputs, Some(&years), 1_767_225_600_000_000)
    );
    with_jumping_clock(|| assert_eq!(decode_batch(&inputs, Some(&years)), expected));
    with_jumping_clock(|| assert_eq!(db.decode_batch(&inputs, Some(&years)), expected));
    let flat = decode_batch_flat_at(&inputs, Some(&years), 0);
    with_jumping_clock(|| assert_eq!(decode_batch_flat(&inputs, Some(&years)), flat));
    with_jumping_clock(|| assert_eq!(db.decode_batch_flat(&inputs, Some(&years)), flat));
    with_jumping_clock(|| {
        assert_eq!(
            decode_batch_json(&inputs, Some(&years)),
            decode_batch_json_at(&inputs, Some(&years), 0)
        )
    });
    with_jumping_clock(|| {
        assert_eq!(
            decode_batch_json_flat(&inputs, Some(&years)),
            decode_batch_json_flat_at(&inputs, Some(&years), 0)
        )
    });
    let metas = resolve_ids(db, &[26]).unwrap();
    with_jumping_clock(|| {
        let actual = decode_batch_ids(&inputs, Some(&years), &metas);
        let expected = decode_batch_ids_at(&inputs, Some(&years), &metas, 0);
        assert_eq!(actual, expected);
    });
}

#[cfg(feature = "arrow")]
#[test]
fn automatic_arrow_decoder_keeps_its_clock_between_batches() {
    use arrow_array::{RecordBatch, StringArray};
    use std::sync::Arc;
    if Db::try_embedded().is_none() {
        return;
    }
    let input = RecordBatch::try_from_iter([(
        "vin",
        Arc::new(StringArray::from(vec![VIN])) as arrow_array::ArrayRef,
    )])
    .unwrap();
    let opts = ArrowOpts {
        columns: vec![ColumnSpec::Id(26)],
        ..Default::default()
    };
    let expected = ArrowDecoder::new_at(&input.schema(), &opts, 0)
        .unwrap()
        .decode_batch(&input)
        .unwrap();
    with_jumping_clock(|| {
        let decoder = ArrowDecoder::new(&input.schema(), &opts).unwrap();
        assert_eq!(decoder.out_schema().metadata()["ultravin.now_micros"], "0");
        assert_eq!(decoder.decode_batch(&input).unwrap(), expected);
        assert_eq!(decoder.decode_batch(&input).unwrap(), expected);
    });
}

#[cfg(feature = "parquet")]
#[test]
fn automatic_parquet_directory_keeps_its_clock_across_files() {
    use arrow_array::{RecordBatch, StringArray};
    use std::sync::Arc;
    if Db::try_embedded().is_none() {
        return;
    }
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory =
        std::env::temp_dir().join(format!("ultravin-auto-clock-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let scratch = Scratch(directory);
    let input = RecordBatch::try_from_iter([(
        "vin",
        Arc::new(StringArray::from(vec![VIN])) as arrow_array::ArrayRef,
    )])
    .unwrap();
    for name in ["a.parquet", "b.parquet"] {
        crate::parquet_io::write_parquet(
            std::iter::once(Ok(input.clone())),
            input.schema(),
            &scratch.0.join(name),
            1,
        )
        .unwrap();
    }
    let opts = crate::parquet_io::ParquetOpts {
        columns: vec![ColumnSpec::Id(26)],
        batch_size: 1,
        ..Default::default()
    };
    let expected: Vec<_> = crate::parquet_io::open_chunks_at(&scratch.0, opts.clone(), 0)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    with_jumping_clock(|| {
        let actual: Vec<_> = crate::parquet_io::open_chunks(&scratch.0, opts)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(actual.len(), 2);
        assert_eq!(actual, expected);
    });
}
