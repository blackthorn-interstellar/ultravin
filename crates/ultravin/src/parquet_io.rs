//! Parquet-to-parquet dataset decode: read row groups, decode + project each
//! chunk through [`crate::arrow_io::ArrowDecoder`], write projected parquet.
//!
//! This is the file-I/O layer over the source-agnostic arrow core — everything
//! here is about getting batches off disk (directory expansion, per-file column
//! resolution including the value sniffers, streaming, writing). The batch→batch
//! decode itself lives in [`crate::arrow_io`] and is shared with every other
//! Arrow source.
//!
//! Memory is O(chunk), never O(file): the reader streams record batches at
//! `batch_size` rows, and the writer emits roughly one row group per chunk. It
//! is also flat in the input's width — the reader is projected down to the VIN
//! and caller-year columns, so a 500-column source costs what a 2-column one
//! does. No row of the output is materialized as a Python object on this path —
//! the pyo3 layer hands back either a row count or lazily-iterated column dicts.
//!
//! [`decode_compact`] is the fast door for big files: it decodes on the native
//! stream, whose result cache hands a repeated vehicle to every VIN that has
//! it, and writes that sharing out instead of undoing it. Each input VIN is a
//! row of `vins.parquet` (the VIN and a `vehicle_id`); each vehicle the stream
//! decoded fresh is one row of `vehicles.parquet`, built on the workers from
//! the full result (`arrow_io::ResultProjection`) and encoded a
//! column per thread. Reading, decoding and the two writers each run on their
//! own threads.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use arrow_array::builder::{ArrayBuilder, StringBuilder};
use arrow_array::types::Int32Type;
use arrow_array::{
    cast::AsArray, Array, ArrayRef, Int32Array, RecordBatch, RecordBatchReader, UInt64Array,
};
use arrow_cast::cast;
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::{compute_leaves, ArrowColumnChunk, ArrowColumnWriter};
use parquet::arrow::{ArrowWriter, ProjectionMask};
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use crate::adaptive::BatchFeedback;
use crate::arrow_io::{
    is_intish, is_stringish, names, vin_by_name, year_by_name, ArrowBatchRebatcher, ArrowDecoder,
    ColumnNames, ResultColumns, ResultProjection,
};
use crate::db::Db;
use crate::ids::{resolve_columns, ColumnSpec, IdMeta};
use crate::predictor::BatchFormat;
use crate::NativeBatch;

/// The dataset door reports failures through the arrow layer's error type, so a
/// column mistake reads the same whether it came off disk or out of a stream.
pub use crate::arrow_io::ArrowError as ParquetError;

/// Caller-tunable knobs for a dataset job.
#[derive(Debug, Clone)]
pub struct ParquetOpts {
    /// Input VIN column name; `None` → autodetect.
    pub vin: Option<String>,
    /// Input caller-year column name; `None` → autodetect (absence is fine).
    pub year: Option<String>,
    /// Elements to project, by id or name, in output order; validated against
    /// the archive by [`open_chunks`].
    pub columns: Vec<ColumnSpec>,
    /// How to label the projected columns; defaults to the variable name.
    pub names: ColumnNames,
    /// Rows per chunk — memory, not throughput.
    pub batch_size: usize,
    /// How many leading rows to sniff when autodetecting columns. Clamped to at
    /// least 1, as `batch_size` is — sniffing zero rows would report every column
    /// as unrecognizable rather than as unsampled.
    pub sample_rows: usize,
}

impl Default for ParquetOpts {
    fn default() -> Self {
        ParquetOpts {
            vin: None,
            year: None,
            columns: Vec::new(),
            names: ColumnNames::default(),
            batch_size: 65_536,
            sample_rows: 100,
        }
    }
}

/// Fraction of non-null sample values that must look like VINs / plausible
/// years for a sniffed candidate to count ("vast majority").
const SNIFF_RATIO_NUM: usize = 9;
const SNIFF_RATIO_DEN: usize = 10;

fn is_vin_like(s: &str) -> bool {
    // vPIC uppercases and rejects I/O/Q as encodables, so lowercase input is
    // still decodable — accept it here rather than failing autodetect on a
    // lowercase export.
    s.len() == 17
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() && !matches!(b.to_ascii_uppercase(), b'I' | b'O' | b'Q')
        })
}

/// Ratio test: at least one non-null value was sampled, and `n_hits` is ≥90% of
/// the `n_total` that were.
/// Shared by both sniffers so the "vast majority" bar lives in one place.
fn ratio_ok(n_total: usize, n_hits: usize) -> bool {
    n_total > 0 && n_hits * SNIFF_RATIO_DEN >= n_total * SNIFF_RATIO_NUM
}

/// Stringish columns of `batch` whose first `sample` non-null values are ≥90%
/// VIN-shaped.
fn sniff_vin_candidates(batch: &RecordBatch, sample: usize) -> Vec<usize> {
    let mut cands = Vec::new();
    for i in 0..batch.num_columns() {
        let dt = batch.column(i).data_type();
        if !is_stringish(dt) {
            continue;
        }
        let Ok(arr) = cast(batch.column(i), &DataType::Utf8) else {
            continue;
        };
        let s = arr.as_string::<i32>();
        let n = sample.min(s.len());
        let (mut non_null, mut hits) = (0usize, 0usize);
        for r in 0..n {
            if s.is_null(r) {
                continue;
            }
            non_null += 1;
            if is_vin_like(s.value(r)) {
                hits += 1;
            }
        }
        if ratio_ok(non_null, hits) {
            cands.push(i);
        }
    }
    cands
}

/// Integer columns whose first `sample` non-null values are ≥90% plausible model
/// years (`[1980, current_year + 2]`, the same window vPIC accepts).
fn sniff_year_candidates_at(batch: &RecordBatch, sample: usize, now_micros: i64) -> Vec<usize> {
    let lo = 1980i64;
    let hi = i64::from(crate::current_year_at(now_micros) + 2);
    let mut cands = Vec::new();
    for i in 0..batch.num_columns() {
        let dt = batch.column(i).data_type();
        if !is_intish(dt) {
            continue;
        }
        // One cast kernel covers every int width/sign.
        let Ok(arr) = cast(batch.column(i), &DataType::Int64) else {
            continue;
        };
        let y = arr.as_primitive::<arrow_array::types::Int64Type>();
        let n = sample.min(y.len());
        let (mut non_null, mut hits) = (0usize, 0usize);
        for r in 0..n {
            if y.is_null(r) {
                continue;
            }
            non_null += 1;
            if (lo..=hi).contains(&y.value(r)) {
                hits += 1;
            }
        }
        if ratio_ok(non_null, hits) {
            cands.push(i);
        }
    }
    cands
}

/// Expand `src` into the ordered list of parquet files to process.
fn expand_src(src: &Path) -> Result<Vec<PathBuf>, ParquetError> {
    if src.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(src)
            .map_err(|e| ParquetError::Io(format!("{}: {e}", src.display())))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "parquet"))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(ParquetError::Config(format!(
                "{} contains no .parquet files",
                src.display()
            )));
        }
        Ok(files)
    } else if src.is_file() {
        Ok(vec![src.to_path_buf()])
    } else {
        Err(ParquetError::Io(format!("{}: not found", src.display())))
    }
}

/// Per-file reader state: the streaming reader, plus the decoder bound to the
/// columns this file resolved.
struct FileState {
    reader: parquet::arrow::arrow_reader::ParquetRecordBatchReader,
    decoder: ArrowDecoder,
    rebatcher: ArrowBatchRebatcher,
    exhausted: bool,
}

impl FileState {
    /// Open one file: footer-based column resolution (names first, then a sniff
    /// over the leading rows), then a streaming reader projected down to the one
    /// or two columns the decode reads.
    fn open(
        path: &Path,
        opts: &ParquetOpts,
        metas: &[IdMeta],
        now_micros: i64,
        reader_batch_size: usize,
    ) -> Result<Self, ParquetError> {
        let file =
            File::open(path).map_err(|e| ParquetError::Io(format!("{}: {e}", path.display())))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
            ParquetError::Io(format!("{}: reading parquet footer: {e}", path.display()))
        })?;
        let schema = builder.schema().clone();

        // The sniff corpus, read at most once per file and only if a name fails
        // to resolve: outer `None` = not read yet, inner `None` = empty file.
        let mut sample: Option<Option<RecordBatch>> = None;

        // Names first — the same resolution every Arrow source gets. Only when
        // they come up empty does the file get sniffed, which is the one thing
        // this layer knows how to do that a bare schema cannot.
        let vin_idx = match vin_by_name(&schema, opts.vin.as_deref())? {
            Some(i) => i,
            None => {
                let b = peek_first(&mut sample, path, opts)?.ok_or_else(|| {
                    ParquetError::Config(format!(
                        "{}: cannot autodetect the VIN column from an empty file — \
                         name the column explicitly",
                        path.display()
                    ))
                })?;
                let cands = sniff_vin_candidates(b, opts.sample_rows.max(1));
                one_candidate(&cands, &schema, "VIN-like")?
            }
        };

        let year_idx = match year_by_name(&schema, opts.year.as_deref())? {
            Some(i) => Some(i),
            None => {
                // No year column is fine — those rows just decode without the
                // caller hint, so an empty file is not fatal here the way a
                // missing VIN column is. Only ambiguity is a mistake.
                let cands = match peek_first(&mut sample, path, opts)? {
                    Some(b) => sniff_year_candidates_at(b, opts.sample_rows.max(1), now_micros),
                    None => Vec::new(),
                };
                if cands.len() > 1 {
                    return Err(ParquetError::Config(format!(
                        "ambiguous caller-year column: {}",
                        names(&schema, &cands)
                    )));
                }
                cands.first().copied()
            }
        };

        // Nothing downstream of here reads any other column, so masking them out
        // keeps a chunk's cost off the input's width. A projected batch holds
        // only the kept columns, still in file order — the indices move with
        // them, so the decoder is bound to the projected schema, not the file's.
        let mut keep = vec![vin_idx];
        keep.extend(year_idx);
        keep.sort_unstable();
        // A column named as both the VIN and the year is a caller mistake the
        // decoder rejects below; don't hand the reader a repeated index to
        // interpret in the meantime.
        keep.dedup();
        let projected = |i: usize| keep.iter().position(|&k| k == i).expect("a kept column");
        let mask = ProjectionMask::roots(builder.parquet_schema(), keep.iter().copied());
        let reader = builder
            .with_projection(mask)
            .with_batch_size(reader_batch_size.max(1))
            .build()
            .map_err(|e| ParquetError::Io(format!("{}: {e}", path.display())))?;

        // Validation (a non-text VIN column, one column claimed twice, a name
        // collision in the output) lives with the decoder, so `vin=` naming a
        // number column fails here exactly as it does for any other source.
        let decoder = ArrowDecoder::with_columns_at(
            &reader.schema(),
            projected(vin_idx),
            year_idx.map(projected),
            metas.to_vec(),
            opts.names,
            now_micros,
        )?;

        Ok(FileState {
            reader,
            decoder,
            rebatcher: ArrowBatchRebatcher::new(),
            exhausted: false,
        })
    }
}

/// The file's leading rows, at full width, cached in `sample` so it is read at
/// most once (outer `None` = not read yet, inner `None` = empty file).
///
/// This is the one read the projection cannot narrow — sniffing has to look at
/// the columns the decode will not touch — so it gets its own throwaway reader,
/// capped at the rows the sniffers actually inspect, and the real reader still
/// starts at row 0.
fn peek_first<'a>(
    sample: &'a mut Option<Option<RecordBatch>>,
    path: &Path,
    opts: &ParquetOpts,
) -> Result<Option<&'a RecordBatch>, ParquetError> {
    if sample.is_none() {
        let file =
            File::open(path).map_err(|e| ParquetError::Io(format!("{}: {e}", path.display())))?;
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| {
                ParquetError::Io(format!("{}: reading parquet footer: {e}", path.display()))
            })?
            .with_batch_size(opts.batch_size.clamp(1, opts.sample_rows.max(1)))
            .build()
            .map_err(|e| ParquetError::Io(format!("{}: {e}", path.display())))?;
        *sample = Some(
            reader
                .next()
                .transpose()
                .map_err(|e| ParquetError::Io(format!("{}: {e}", path.display())))?,
        );
    }
    Ok(sample.as_ref().and_then(Option::as_ref))
}

/// The passthrough (VIN, optional caller-year) column names of an output schema
/// — the only part two files in a directory can disagree about, since every file
/// projects the same id list. The layout is `[vin, year?, decoded_model_year,
/// ..projected]`, so the passthrough prefix is what the projection leaves over.
fn passthrough_names(schema: &SchemaRef, n_projected: usize) -> String {
    let n = schema.fields().len().saturating_sub(n_projected + 1);
    schema
        .fields()
        .iter()
        .take(n)
        .map(|f| f.name().as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn one_candidate(cands: &[usize], schema: &SchemaRef, what: &str) -> Result<usize, ParquetError> {
    match cands {
        [i] => Ok(*i),
        [] => Err(ParquetError::Config(format!(
            "could not autodetect a {what} column — pass the column name explicitly"
        ))),
        many => Err(ParquetError::Config(format!(
            "ambiguous {what} column candidates: {}",
            names(schema, many)
        ))),
    }
}

/// Streaming decoder over one file or a directory of them. Eagerly opens the
/// first file so the output schema is known before the first chunk flows —
/// callers writing parquet need it even for zero-row inputs.
pub struct ParquetChunkIter {
    files: std::vec::IntoIter<PathBuf>,
    state: Option<FileState>,
    opts: ParquetOpts,
    metas: Vec<IdMeta>,
    now_micros: i64,
    feedback: Option<BatchFeedback>,
    /// Set once a chunk or a file open has failed. The reader is then parked
    /// mid-source, so resuming would silently skip whatever it could not read —
    /// stop instead.
    failed: bool,
    /// Output schema (present once the first file is open).
    pub out_schema: Option<SchemaRef>,
}

/// Open a dataset source for chunked decoding.
pub fn open_chunks(src: &Path, opts: ParquetOpts) -> Result<ParquetChunkIter, ParquetError> {
    open_chunks_at(src, opts, crate::now_micros())
}

/// [`open_chunks`] at one explicit instant, shared by every file and batch.
pub fn open_chunks_at(
    src: &Path,
    opts: ParquetOpts,
    now_micros: i64,
) -> Result<ParquetChunkIter, ParquetError> {
    open_chunks_impl(src, opts, now_micros, None)
}

/// Open a dataset with batch sizes learned from the work itself, at one stable
/// instant shared by the whole dataset.
pub fn open_chunks_auto_at(
    src: &Path,
    opts: ParquetOpts,
    memory_bytes: usize,
    now_micros: i64,
) -> Result<ParquetChunkIter, ParquetError> {
    let feedback = BatchFeedback::new_predictive(
        BatchFormat::Columnar,
        memory_bytes,
        rayon::current_num_threads(),
    )
    .map_err(|e| ParquetError::Config(e.to_string()))?;
    open_chunks_impl(src, opts, now_micros, Some(feedback))
}

fn open_chunks_impl(
    src: &Path,
    opts: ParquetOpts,
    now_micros: i64,
    feedback: Option<BatchFeedback>,
) -> Result<ParquetChunkIter, ParquetError> {
    let metas = resolve_columns(Db::embedded(), &opts.columns).map_err(ParquetError::Config)?;
    let mut iter = ParquetChunkIter {
        files: expand_src(src)?.into_iter(),
        state: None,
        opts,
        metas,
        now_micros,
        feedback,
        failed: false,
        out_schema: None,
    };
    iter.advance_file()?;
    Ok(iter)
}

impl ParquetChunkIter {
    pub fn feedback(&self) -> Option<BatchFeedback> {
        self.feedback.clone()
    }

    /// Open the next file, if any. `Ok(false)` = exhausted.
    ///
    /// Every file in a directory must resolve the same output shape. They are
    /// autodetected independently, so one part file carrying a `year` column its
    /// siblings lack would change the schema mid-stream: the dict form would
    /// yield differently-keyed chunks and the parquet writer, which zips columns
    /// positionally against the schema it opened with, would write a passthrough
    /// year into `decoded_model_year`. Refuse instead, in one place, so all three
    /// output modes fail the same way.
    fn advance_file(&mut self) -> Result<bool, ParquetError> {
        let Some(path) = self.files.next() else {
            return Ok(false);
        };
        let reader_batch_size = if self.feedback.is_some() {
            8_192
        } else {
            self.opts.batch_size.max(1)
        };
        let st = FileState::open(
            &path,
            &self.opts,
            &self.metas,
            self.now_micros,
            reader_batch_size,
        )?;
        let schema = st.decoder.out_schema();
        match &self.out_schema {
            Some(first) if first != schema => {
                return Err(ParquetError::Config(format!(
                    "{}: passes through [{}], but the first file passes through [{}] — \
                     every file in a directory must resolve the same columns; \
                     name the VIN/year columns explicitly",
                    path.display(),
                    passthrough_names(schema, self.metas.len()),
                    passthrough_names(first, self.metas.len()),
                )))
            }
            Some(_) => {}
            None => self.out_schema = Some(schema.clone()),
        }
        self.state = Some(st);
        Ok(true)
    }

    /// Pull, decode, and project the next chunk. Fused: once a call has failed,
    /// every later one reports exhaustion rather than resuming past the gap.
    pub fn next_chunk(&mut self) -> Result<Option<RecordBatch>, ParquetError> {
        if self.failed {
            return Ok(None);
        }
        let r = self.next_chunk_inner();
        self.failed = r.is_err();
        r
    }

    fn next_chunk_inner(&mut self) -> Result<Option<RecordBatch>, ParquetError> {
        if self.feedback.is_none() {
            return match self.next_input()? {
                Some((batch, decoder)) => decoder.decode_batch(&batch).map(Some),
                None => Ok(None),
            };
        }
        loop {
            if self.state.is_none() && !self.advance_file()? {
                return Ok(None);
            }
            if let Some(batch) = self.next_adaptive_batch()? {
                return Ok(Some(batch));
            }
            self.state = None;
        }
    }

    /// The next batch as read, the VIN and year columns only, with the
    /// decoder bound to its file.
    fn next_input(&mut self) -> Result<Option<(RecordBatch, &ArrowDecoder)>, ParquetError> {
        loop {
            if self.state.is_none() && !self.advance_file()? {
                return Ok(None);
            }
            let st = self.state.as_mut().expect("a file is open");
            match st.reader.next().transpose() {
                Err(e) => return Err(ParquetError::Io(format!("reading row group: {e}"))),
                Ok(Some(batch)) => {
                    let st = self.state.as_ref().expect("a file is open");
                    return Ok(Some((batch, &st.decoder)));
                }
                // File drained — fall through to the next one, if any.
                Ok(None) => self.state = None,
            }
        }
    }

    fn next_adaptive_batch(&mut self) -> Result<Option<RecordBatch>, ParquetError> {
        let feedback = self.feedback.as_ref().expect("adaptive mode").clone();
        let target = feedback.next_rows();
        let started = Instant::now();
        let input = loop {
            let st = self.state.as_mut().expect("a file is open");
            if let Some(batch) = st.rebatcher.take(target)? {
                break Some(batch);
            }
            if st.exhausted {
                break st.rebatcher.finish()?;
            }
            match st.reader.next().transpose() {
                Err(e) => return Err(ParquetError::Io(format!("reading row group: {e}"))),
                Ok(Some(batch)) => st.rebatcher.push(batch),
                Ok(None) => st.exhausted = true,
            }
        };
        let Some(input) = input else {
            return Ok(None);
        };
        let rows = input.num_rows();
        let decoder = &self.state.as_ref().expect("a file is open").decoder;
        let output = feedback.decode(rows, || {
            let output = decoder.decode_batch(&input)?;
            let output_bytes = decoder.batch_working_bytes(&output);
            Ok::<_, ParquetError>((output, output_bytes))
        })?;
        feedback.observe_total(
            rows,
            started.elapsed(),
            decoder.batch_working_bytes(&output),
        );
        Ok(Some(output))
    }
}

impl Iterator for ParquetChunkIter {
    type Item = Result<RecordBatch, ParquetError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_chunk().transpose()
    }
}

/// Where `dst` would land, whether or not it exists yet: an existing path
/// resolved, otherwise its resolved parent plus the file name. `None` when
/// neither resolves — a source that is not there and a destination that cannot
/// be created are both reported later, with better messages.
fn resolve_dst(dst: &Path) -> Option<PathBuf> {
    if let Ok(p) = dst.canonicalize() {
        return Some(p);
    }
    let parent = match dst.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => return None,
    };
    Some(parent.canonicalize().ok()?.join(dst.file_name()?))
}

/// Keep source paths separate from generated output. Replacing the source loses
/// the original input, and adding output inside a source directory changes what
/// the next job reads. Refuse both before consuming the stream.
pub fn check_dst_outside_src(src: &Path, dst: &Path) -> Result<(), ParquetError> {
    let (Ok(src_real), Some(dst_real)) = (src.canonicalize(), resolve_dst(dst)) else {
        return Ok(());
    };
    if src_real.is_dir() {
        if dst_real.starts_with(&src_real) {
            return Err(ParquetError::Config(format!(
                "destination {} is inside the source directory {} — write it elsewhere",
                dst.display(),
                src.display()
            )));
        }
    } else if dst_real == src_real {
        return Err(ParquetError::Config(format!(
            "destination {} is the source being decoded — write it elsewhere",
            dst.display()
        )));
    }
    Ok(())
}

/// Decode a whole source into `dst`, returning the rows written.
pub fn decode_parquet_to_file(
    src: &Path,
    dst: &Path,
    opts: ParquetOpts,
) -> Result<usize, ParquetError> {
    check_dst_outside_src(src, dst)?;
    let row_group = opts.batch_size.max(1);
    let iter = open_chunks(src, opts)?;
    let schema = iter
        .out_schema
        .clone()
        .ok_or_else(|| ParquetError::Config("source expanded to no files".to_string()))?;
    write_parquet(iter, schema, dst, row_group)
}

/// Write every batch a reader yields to `dst` as Snappy parquet, one row group
/// per batch; returns the rows written.
///
/// The reader's schema opens the file, so a zero-row source still writes a valid
/// (empty) parquet with the right columns. One row group per batch keeps peak
/// write-side memory at O(batch), matching the read side.
pub fn write_parquet(
    batches: impl Iterator<Item = Result<RecordBatch, ParquetError>>,
    schema: SchemaRef,
    dst: &Path,
    row_group: usize,
) -> Result<usize, ParquetError> {
    ParquetSink::create(schema, dst, row_group)?.write_all(batches)
}

pub fn write_parquet_adaptive(
    batches: impl Iterator<Item = Result<RecordBatch, ParquetError>>,
    schema: SchemaRef,
    dst: &Path,
    feedback: BatchFeedback,
) -> Result<usize, ParquetError> {
    ParquetSink::create_adaptive(schema, dst, feedback)?.write_all(batches)
}

/// A parquet destination that is ready to take batches: the temporary file
/// beside `dst` is reserved and the writer built. Opening it before taking a
/// one-shot source means a bad destination fails while the source is unread.
pub struct ParquetSink {
    writer: ArrowWriter<std::fs::File>,
    tmp: TempOutput,
    dst: PathBuf,
    destination_permissions: Option<std::fs::Permissions>,
    feedback: Option<BatchFeedback>,
}

impl ParquetSink {
    /// One row group per `row_group` rows, as [`write_parquet`] writes.
    pub fn create(schema: SchemaRef, dst: &Path, row_group: usize) -> Result<Self, ParquetError> {
        Self::open(schema, dst, row_group, None)
    }

    /// As [`write_parquet_adaptive`] writes, timing each flush into `feedback`.
    pub fn create_adaptive(
        schema: SchemaRef,
        dst: &Path,
        feedback: BatchFeedback,
    ) -> Result<Self, ParquetError> {
        Self::open(schema, dst, 65_536, Some(feedback))
    }

    fn open(
        schema: SchemaRef,
        dst: &Path,
        row_group: usize,
        feedback: Option<BatchFeedback>,
    ) -> Result<Self, ParquetError> {
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .set_max_row_group_row_count(Some(row_group.max(1)))
            .build();
        Self::with_props(schema, dst, props, feedback)
    }

    fn with_props(
        schema: SchemaRef,
        dst: &Path,
        props: WriterProperties,
        feedback: Option<BatchFeedback>,
    ) -> Result<Self, ParquetError> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

        let parent = dst.parent().unwrap_or_else(|| Path::new("."));
        let (tmp, out) = (0..100)
            .find_map(|_| {
                let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let tmp = parent.join(format!(".ultravin-{}-{nonce}.tmp", std::process::id()));
                match OpenOptions::new().write(true).create_new(true).open(&tmp) {
                    Ok(file) => Some(Ok((tmp, file))),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(ParquetError::Io(format!("{}: {e}", tmp.display())))),
                }
            })
            .unwrap_or_else(|| {
                Err(ParquetError::Io(format!(
                    "could not reserve a temporary file next to {}",
                    dst.display()
                )))
            })?;
        let tmp = TempOutput(tmp);
        let destination_permissions = std::fs::metadata(dst)
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.permissions());
        let writer = ArrowWriter::try_new(out, schema, Some(props))
            .map_err(|e| ParquetError::Io(format!("{}: {e}", tmp.0.display())))?;
        Ok(Self {
            writer,
            tmp,
            dst: dst.to_path_buf(),
            destination_permissions,
            feedback,
        })
    }

    /// Write every batch, then install the finished file at `dst`; returns the
    /// rows written. Any error leaves `dst` untouched and removes the temp file.
    pub fn write_all(
        mut self,
        batches: impl Iterator<Item = Result<RecordBatch, ParquetError>>,
    ) -> Result<usize, ParquetError> {
        let mut rows = 0usize;
        for batch in batches {
            let batch = batch?;
            rows += batch.num_rows();
            let started = Instant::now();
            self.writer
                .write(&batch)
                .map_err(|e| ParquetError::Io(format!("writing row group: {e}")))?;
            if let Some(feedback) = &self.feedback {
                self.writer
                    .flush()
                    .map_err(|e| ParquetError::Io(format!("flushing row group: {e}")))?;
                feedback.add_output_time(started.elapsed());
            }
        }
        let tmp = &self.tmp.0;
        self.writer
            .close()
            .map_err(|e| ParquetError::Io(format!("finalizing {}: {e}", tmp.display())))?;
        Written::new(self.tmp, self.dst, self.destination_permissions)?.install()?;
        Ok(rows)
    }

    /// Write every batch, each row group of about `row_group` rows buffered
    /// and its columns encoded in parallel on `encoders` (with many columns,
    /// encoding is the work, and it splits by column); then finish the temp
    /// file but leave `dst` alone until [`Written::install`].
    fn write_parallel(
        self,
        batches: impl Iterator<Item = Result<RecordBatch, ParquetError>>,
        row_group: usize,
        encoders: &rayon::ThreadPool,
    ) -> Result<(usize, Written), ParquetError> {
        let io =
            |e: parquet::errors::ParquetError| ParquetError::Io(format!("writing row group: {e}"));
        let (mut file, columns) = self.writer.into_serialized_writer().map_err(io)?;
        let (mut rows, mut groups) = (0, 0);
        let mut pending: Vec<RecordBatch> = Vec::new();
        let mut batches = batches.peekable();
        while let Some(batch) = batches.next() {
            let batch = batch?;
            rows += batch.num_rows();
            pending.push(batch);
            let buffered: usize = pending.iter().map(RecordBatch::num_rows).sum();
            if buffered < row_group && batches.peek().is_some() {
                continue;
            }
            let writers = columns.create_column_writers(groups).map_err(io)?;
            let chunks = encode_columns(writers, &pending, encoders)?;
            let mut group = file.next_row_group().map_err(io)?;
            for chunk in chunks {
                chunk.append_to_row_group(&mut group).map_err(io)?;
            }
            group.close().map_err(io)?;
            pending.clear();
            groups += 1;
        }
        let tmp = &self.tmp.0;
        file.close()
            .map_err(|e| ParquetError::Io(format!("finalizing {}: {e}", tmp.display())))?;
        let written = Written::new(self.tmp, self.dst, self.destination_permissions)?;
        Ok((rows, written))
    }
}

/// A finished parquet file waiting beside its destination; dropping it
/// removes the file.
struct Written {
    tmp: TempOutput,
    dst: PathBuf,
}

impl Written {
    /// A closed temp file, given the permissions of the file it replaces.
    fn new(
        tmp: TempOutput,
        dst: PathBuf,
        permissions: Option<std::fs::Permissions>,
    ) -> Result<Self, ParquetError> {
        if let Some(permissions) = permissions {
            std::fs::set_permissions(&tmp.0, permissions).map_err(|e| {
                ParquetError::Io(format!("setting permissions on {}: {e}", tmp.0.display()))
            })?;
        }
        Ok(Self { tmp, dst })
    }

    fn install(self) -> Result<(), ParquetError> {
        std::fs::rename(&self.tmp.0, &self.dst).map_err(|e| {
            ParquetError::Io(format!(
                "installing completed parquet at {}: {e}",
                self.dst.display()
            ))
        })?;
        self.tmp.keep();
        Ok(())
    }
}

/// Removes an incomplete output on every error path. The destination itself is
/// untouched until the parquet footer has been written successfully.
struct TempOutput(PathBuf);

impl TempOutput {
    fn keep(mut self) {
        self.0.clear();
    }
}

impl Drop for TempOutput {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            #[cfg(windows)]
            if let Ok(metadata) = std::fs::metadata(&self.0) {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(&self.0, permissions);
            }
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// Encode column `i` of every batch with `writers[i]` on `encoders`, the
/// columns in parallel, and return the finished chunks in order. Every column
/// is flat, one parquet leaf each.
fn encode_columns(
    writers: Vec<ArrowColumnWriter>,
    batches: &[RecordBatch],
    encoders: &rayon::ThreadPool,
) -> Result<Vec<ArrowColumnChunk>, ParquetError> {
    use rayon::prelude::*;
    let schema = batches[0].schema();
    assert_eq!(writers.len(), schema.fields().len(), "one leaf per column");
    let encode = |(i, mut writer): (usize, ArrowColumnWriter)| {
        for batch in batches {
            for leaf in compute_leaves(schema.field(i), batch.column(i))? {
                writer.write(&leaf)?;
            }
        }
        writer.close()
    };
    encoders
        .install(|| {
            writers
                .into_par_iter()
                .enumerate()
                .map(encode)
                .collect::<Result<_, _>>()
        })
        .map_err(|e| ParquetError::Io(format!("encoding column: {e}")))
}

/// The VIN file a compact decode writes: one row per input VIN, in input
/// order — the VIN as given and its [`VEHICLE_ID`].
pub const COMPACT_VINS: &str = "vins.parquet";
/// The vehicle file a compact decode writes: one row per [`VEHICLE_ID`],
/// carrying every other column [`decode_parquet_to_file`] writes.
pub const COMPACT_VEHICLES: &str = "vehicles.parquet";
/// The column joining [`COMPACT_VINS`] to [`COMPACT_VEHICLES`].
pub const VEHICLE_ID: &str = "vehicle_id";

/// Input rows a compact decode streams at once. Each chunk is one native
/// stream and one row group of [`COMPACT_VINS`]: 512Ki rows ran as fast as
/// 1Mi on sorted NY and held ~0.2 GB less; 4Mi was slower.
const COMPACT_CHUNK_ROWS: usize = 1 << 19;
/// Vehicle rows gathered into one batch before they are handed on: a stream
/// batch adds a few, and building columns costs per column.
const VEHICLE_BATCH_ROWS: usize = 2_048;
/// Rows per row group of [`COMPACT_VEHICLES`], buffered whole while its
/// columns encode in parallel.
const VEHICLE_ROW_GROUP: usize = 32_768;

/// The rows a compact decode wrote to each file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactRows {
    pub vins: usize,
    pub vehicles: usize,
}

/// Decode `src` into [`COMPACT_VINS`] and [`COMPACT_VEHICLES`] in `dst_dir`.
///
/// `src` is a parquet file or directory, its VIN and caller-year columns
/// found as [`decode_parquet_to_file`] finds them, or any other file, read
/// as one VIN per line. A VIN that repeats a vehicle the decode has seen
/// (the same result but for the serial) gets that vehicle's id instead of
/// another row of vehicle columns, so `vins JOIN vehicles USING
/// (vehicle_id)` is [`decode_parquet_to_file`]'s output, row for row. Ids
/// are unique within the output but not dense, and a vehicle can appear
/// under more than one id. `opts.batch_size` is not used: the decode
/// streams in chunks of its own. Memory is bounded by the chunk, not the
/// input. Neither file is replaced unless both are written.
pub fn decode_compact(
    src: &Path,
    dst_dir: &Path,
    opts: ParquetOpts,
) -> Result<CompactRows, ParquetError> {
    decode_compact_at(src, dst_dir, opts, crate::now_micros())
}

/// [`decode_compact`] at one explicit instant.
pub fn decode_compact_at(
    src: &Path,
    dst_dir: &Path,
    opts: ParquetOpts,
    now_micros: i64,
) -> Result<CompactRows, ParquetError> {
    compact(src, dst_dir, opts, now_micros, COMPACT_CHUNK_ROWS)
}

/// One chunk of input: the VIN column as text and the caller years.
struct CompactChunk {
    vins: ArrayRef,
    years: Option<ArrayRef>,
}

enum CompactInput {
    Parquet(Box<ParquetChunkIter>),
    Text(TextLines),
}

impl CompactInput {
    /// Open `src` with the schema [`decode_parquet_to_file`] would write.
    fn open(
        src: &Path,
        opts: ParquetOpts,
        metas: &[IdMeta],
        now_micros: i64,
        chunk_rows: usize,
    ) -> Result<(Self, SchemaRef), ParquetError> {
        if is_parquet(src)? {
            let opts = ParquetOpts {
                batch_size: chunk_rows,
                ..opts
            };
            let iter = open_chunks_impl(src, opts, now_micros, None)?;
            let schema = iter
                .out_schema
                .clone()
                .ok_or_else(|| ParquetError::Config("source expanded to no files".to_string()))?;
            return Ok((Self::Parquet(Box::new(iter)), schema));
        }
        let file =
            File::open(src).map_err(|e| ParquetError::Io(format!("{}: {e}", src.display())))?;
        let schema = Arc::new(Schema::new(vec![Field::new("vin", DataType::Utf8, true)]));
        let decoder = ArrowDecoder::with_columns_at(
            &schema,
            0,
            None,
            metas.to_vec(),
            opts.names,
            now_micros,
        )?;
        let lines = TextLines {
            file,
            buf: Vec::new(),
            pos: 0,
            eof: false,
            line: 0,
        };
        Ok((Self::Text(lines), decoder.out_schema().clone()))
    }

    fn next(&mut self, rows: usize) -> Result<Option<CompactChunk>, ParquetError> {
        match self {
            Self::Text(lines) => Ok(lines
                .next(rows)?
                .map(|vins| CompactChunk { vins, years: None })),
            Self::Parquet(iter) => {
                let Some((batch, decoder)) = iter.next_input()? else {
                    return Ok(None);
                };
                let vins = cast(batch.column(decoder.vin_index()), &DataType::Utf8)
                    .map_err(|e| ParquetError::Io(format!("casting VIN column: {e}")))?;
                let years = decoder
                    .year_index()
                    .map(|yi| cast(batch.column(yi), &DataType::Int32))
                    .transpose()
                    .map_err(|e| {
                        ParquetError::Io(format!("casting caller-year column to int: {e}"))
                    })?;
                Ok(Some(CompactChunk { vins, years }))
            }
        }
    }
}

/// A directory, or a file that starts with parquet's magic.
fn is_parquet(src: &Path) -> Result<bool, ParquetError> {
    if src.is_dir() {
        return Ok(true);
    }
    let mut magic = [0u8; 4];
    match File::open(src).and_then(|mut file| file.read_exact(&mut magic)) {
        Ok(()) => Ok(&magic == b"PAR1"),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ParquetError::Io(format!("{}: not found", src.display())))
        }
        Err(e) => Err(ParquetError::Io(format!("{}: {e}", src.display()))),
    }
}

/// A text file read as one VIN per line; a `\r` before the `\n` is dropped.
struct TextLines {
    file: File,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    /// Lines read so far.
    line: usize,
}

impl TextLines {
    /// The next `rows` lines, fewer at the end, `None` after it.
    fn next(&mut self, rows: usize) -> Result<Option<ArrayRef>, ParquetError> {
        let mut vins = StringBuilder::with_capacity(rows, rows * 17);
        while vins.len() < rows {
            let rest = &self.buf[self.pos..];
            let end = match memchr::memchr(b'\n', rest) {
                Some(end) => end,
                None if self.eof && rest.is_empty() => break,
                None if self.eof => rest.len(),
                None => {
                    self.fill()?;
                    continue;
                }
            };
            let line = &rest[..end];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = std::str::from_utf8(line)
                .map_err(|_| ParquetError::Io(format!("line {} is not UTF-8", self.line + 1)))?;
            vins.append_value(line);
            self.line += 1;
            self.pos += (end + 1).min(rest.len());
        }
        Ok((vins.len() > 0).then(|| Arc::new(vins.finish()) as ArrayRef))
    }

    fn fill(&mut self) -> Result<(), ParquetError> {
        self.buf.drain(..self.pos);
        self.pos = 0;
        let len = self.buf.len();
        self.buf.resize(len + (4 << 20), 0);
        let read = loop {
            match self.file.read(&mut self.buf[len..]) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                read => break read,
            }
        };
        let read = read.map_err(|e| ParquetError::Io(format!("reading input: {e}")))?;
        self.buf.truncate(len + read);
        self.eof = read == 0;
        Ok(())
    }
}

/// Vehicle rows being gathered, a few per stream batch, on whichever worker
/// takes them from the pool: building columns costs per column, so they are
/// built for many rows at once.
struct VehicleRows {
    ids: Vec<u64>,
    years: Vec<Option<i32>>,
    columns: ResultColumns,
}

/// The vehicle rows of a compact decode: the projection, the output schema,
/// and the rows being gathered.
struct Vehicles<'a> {
    projection: &'a ResultProjection,
    schema: SchemaRef,
    has_year: bool,
    pool: std::sync::Mutex<Vec<VehicleRows>>,
}

impl Vehicles<'_> {
    fn take(&self) -> VehicleRows {
        let pooled = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop();
        pooled.unwrap_or_else(|| VehicleRows {
            ids: Vec::with_capacity(VEHICLE_BATCH_ROWS),
            years: Vec::new(),
            columns: self.projection.columns(VEHICLE_BATCH_ROWS),
        })
    }

    fn put(&self, rows: VehicleRows) {
        self.pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(rows);
    }

    /// Gather a batch's fresh rows, and return a batch of them once there
    /// are enough.
    fn gather(
        &self,
        batch: &NativeBatch<'_, '_>,
        years: Option<&[Option<i32>]>,
    ) -> Option<RecordBatch> {
        if !batch.fresh().contains(&true) {
            return None;
        }
        let mut rows = self.take();
        let fresh = batch
            .input_indices()
            .zip(batch.result_ids())
            .zip(batch.fresh())
            .zip(batch.iter());
        for (((index, &id), &fresh), result) in fresh {
            if fresh {
                rows.ids.push(id);
                rows.years.extend(years.map(|years| years[index]));
                self.projection.push(&mut rows.columns, result);
            }
        }
        let full = (rows.ids.len() >= VEHICLE_BATCH_ROWS).then(|| self.finish(&mut rows));
        self.put(rows);
        full
    }

    /// The rows gathered in `rows` as a batch, leaving it empty.
    fn finish(&self, rows: &mut VehicleRows) -> RecordBatch {
        let mut columns: Vec<ArrayRef> =
            vec![Arc::new(UInt64Array::from(std::mem::take(&mut rows.ids)))];
        if self.has_year {
            columns.push(Arc::new(Int32Array::from(std::mem::take(&mut rows.years))));
        }
        columns.extend(rows.columns.finish());
        RecordBatch::try_new(self.schema.clone(), columns).expect("vehicle rows match their schema")
    }

    /// Every batch still being gathered.
    fn drain(&self) -> Vec<RecordBatch> {
        let pool = std::mem::take(&mut *self.pool.lock().unwrap_or_else(|p| p.into_inner()));
        pool.into_iter()
            .filter(|rows| !rows.ids.is_empty())
            .map(|mut rows| self.finish(&mut rows))
            .collect()
    }
}

fn compact(
    src: &Path,
    dst_dir: &Path,
    opts: ParquetOpts,
    now_micros: i64,
    chunk_rows: usize,
) -> Result<CompactRows, ParquetError> {
    let metas = resolve_columns(Db::embedded(), &opts.columns).map_err(ParquetError::Config)?;
    let (mut input, wide) = CompactInput::open(src, opts, &metas, now_micros, chunk_rows)?;
    if let Some(field) = wide.fields().iter().find(|f| f.name() == VEHICLE_ID) {
        return Err(ParquetError::Config(format!(
            "input column {:?} collides with the {VEHICLE_ID} column of a compact decode; \
             rename it",
            field.name()
        )));
    }
    let id_field = Arc::new(Field::new(VEHICLE_ID, DataType::UInt64, false));
    let vin_field = wide.field(0).clone();
    let vins_schema = Arc::new(Schema::new_with_metadata(
        vec![Arc::new(vin_field.clone()), id_field.clone()],
        wide.metadata().clone(),
    ));
    let mut fields = vec![id_field];
    fields.extend(wide.fields().iter().skip(1).cloned());
    let vehicles_schema = Arc::new(Schema::new_with_metadata(fields, wide.metadata().clone()));

    // Made first, so the checks see where the files really land.
    std::fs::create_dir_all(dst_dir)
        .map_err(|e| ParquetError::Io(format!("{}: {e}", dst_dir.display())))?;
    let vins_dst = dst_dir.join(COMPACT_VINS);
    let vehicles_dst = dst_dir.join(COMPACT_VEHICLES);
    check_dst_outside_src(src, &vins_dst)?;
    check_dst_outside_src(src, &vehicles_dst)?;
    let props = |row_group: usize| {
        WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .set_max_row_group_row_count(Some(row_group))
    };
    // VINs are all distinct: a dictionary would only be built to be dropped.
    let vins_props = props(chunk_rows)
        .set_column_dictionary_enabled(ColumnPath::from(vin_field.name().as_str()), false)
        .build();
    let vins_sink = ParquetSink::with_props(vins_schema.clone(), &vins_dst, vins_props, None)?;
    let vehicles_props = props(VEHICLE_ROW_GROUP).build();
    let vehicles_sink =
        ParquetSink::with_props(vehicles_schema.clone(), &vehicles_dst, vehicles_props, None)?;

    let projection = ResultProjection::new(&metas);
    // The wide layout is `[vin, year?, decoded_model_year, ..projected]`.
    let has_year = wide.fields().len() == metas.len() + 3;
    let vehicle_rows = Vehicles {
        projection: &projection,
        schema: vehicles_schema,
        has_year,
        pool: std::sync::Mutex::new(Vec::new()),
    };
    // As many encoders as decode workers, for both files, alive for the call.
    let encoders = rayon::ThreadPoolBuilder::new()
        .num_threads(crate::predictor::worker_count())
        .build()
        .map_err(|e| ParquetError::Io(format!("starting parquet encoders: {e}")))?;
    let encoders = &encoders;
    std::thread::scope(|scope| {
        let (chunks_tx, chunks) = mpsc::sync_channel(1);
        let reader = scope.spawn(move || loop {
            let Some(chunk) = input.next(chunk_rows).transpose() else {
                break;
            };
            let failed = chunk.is_err();
            if chunks_tx.send(chunk).is_err() || failed {
                break;
            }
        });
        let (vins_tx, vins_rx) = mpsc::sync_channel(1);
        let vins = scope.spawn(move || vins_sink.write_parallel(vins_rx.into_iter(), 1, encoders));
        let (vehicles_tx, vehicles_rx) = mpsc::sync_channel(4);
        let vehicles = scope.spawn(move || {
            vehicles_sink.write_parallel(vehicles_rx.into_iter(), VEHICLE_ROW_GROUP, encoders)
        });
        let feeds = Feeds {
            vins: vins_tx,
            vehicles: vehicles_tx,
        };
        let decoded = compact_chunks(chunks, &vehicle_rows, &vins_schema, now_micros, &feeds);
        // A writer that is handed the error finishes no file.
        if let Err(e) = &decoded {
            let _ = feeds.vins.send(Err(e.clone()));
            let _ = feeds.vehicles.send(Err(e.clone()));
        }
        drop(feeds);
        fn join<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
            handle
                .join()
                .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
        }
        // A reader that panicked ended the input early.
        join(reader);
        let (vins, vehicles) = (join(vins), join(vehicles));
        if let Err(e) = decoded {
            // A writer that failed on its own is why the decode stopped.
            let own = [&vins, &vehicles]
                .into_iter()
                .find_map(|written| written.as_ref().err().filter(|w| **w != e));
            return Err(own.cloned().unwrap_or(e));
        }
        let (vin_rows, vins) = vins?;
        let (vehicle_rows, vehicles) = vehicles?;
        vehicles.install()?;
        vins.install()?;
        Ok(CompactRows {
            vins: vin_rows,
            vehicles: vehicle_rows,
        })
    })
}

/// The writers' inboxes.
struct Feeds {
    vins: mpsc::SyncSender<Result<RecordBatch, ParquetError>>,
    vehicles: mpsc::SyncSender<Result<RecordBatch, ParquetError>>,
}

/// Stream each chunk, sending vehicle rows as they gather and each chunk's
/// VINs with their ids once it is done.
fn compact_chunks(
    chunks: mpsc::Receiver<Result<CompactChunk, ParquetError>>,
    vehicles: &Vehicles<'_>,
    vins_schema: &SchemaRef,
    now_micros: i64,
    feeds: &Feeds,
) -> Result<(), ParquetError> {
    let stopped = || ParquetError::Io("a compact output writer stopped".to_string());
    for chunk in chunks {
        let CompactChunk { vins, years } = chunk?;
        let inputs: Vec<&str> = vins
            .as_string::<i32>()
            .iter()
            .map(|vin| vin.unwrap_or(""))
            .collect();
        let years: Option<Vec<Option<i32>>> =
            years.map(|years| years.as_primitive::<Int32Type>().iter().collect());
        let years = years.as_deref();
        let mut ids = vec![0u64; inputs.len()];
        let mut sent = true;
        let convert = |batch: &NativeBatch<'_, 'static>| vehicles.gather(batch, years);
        Db::embedded()
            .batch_stream_at(&inputs, years, now_micros, convert, |batch, gathered| {
                for (index, &id) in batch.input_indices().zip(batch.result_ids()) {
                    ids[index] = id;
                }
                if let Some(gathered) = gathered {
                    sent &= feeds.vehicles.send(Ok(gathered)).is_ok();
                }
            })
            .map_err(|e| ParquetError::Io(e.to_string()))?;
        drop(inputs);
        if !sent {
            return Err(stopped());
        }
        let batch = RecordBatch::try_new(
            vins_schema.clone(),
            vec![vins, Arc::new(UInt64Array::from(ids))],
        )
        .map_err(|e| ParquetError::Io(format!("assembling VIN rows: {e}")))?;
        feeds.vins.send(Ok(batch)).map_err(|_| stopped())?;
    }
    for gathered in vehicles.drain() {
        feeds.vehicles.send(Ok(gathered)).map_err(|_| stopped())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int32Array, StringArray};
    use arrow_schema::{Field, Schema};

    const MAKE: i32 = 26; // lookup  -> Utf8
    const CYLINDERS: i32 = 9; // int     -> Int64
    const DISPLACEMENT_L: i32 = 13; // decimal -> Float64

    const HONDA: &str = "1HGCM82633A004352";
    const FORD: &str = "1FTFW1ET5DFC10312";
    /// Registered WMI, undecodable descriptor — a miss that must still be a row.
    const MISS: &str = "ZZZCM82633A004352";

    fn loaded() -> bool {
        Db::try_embedded().is_some()
    }

    /// A scratch directory that cleans up after itself. The crate carries no
    /// tempfile dev-dependency and these tests need real files on disk.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ultravin-parquet-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn opts(ids: &[i32]) -> ParquetOpts {
        ParquetOpts {
            columns: ids.iter().copied().map(ColumnSpec::from).collect(),
            ..ParquetOpts::default()
        }
    }

    fn utf8(name: &str, values: &[Option<&str>]) -> (Field, ArrayRef) {
        (
            Field::new(name, DataType::Utf8, true),
            Arc::new(StringArray::from(values.to_vec())),
        )
    }

    fn i32s(name: &str, values: &[Option<i32>]) -> (Field, ArrayRef) {
        (
            Field::new(name, DataType::Int32, true),
            Arc::new(Int32Array::from(values.to_vec())),
        )
    }

    fn batch(cols: Vec<(Field, ArrayRef)>) -> RecordBatch {
        let (fields, arrays): (Vec<_>, Vec<_>) = cols.into_iter().unzip();
        RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).expect("input batch")
    }

    fn write(path: &Path, b: &RecordBatch) {
        let f = File::create(path).expect("create");
        let mut w = ArrowWriter::try_new(f, b.schema(), None).expect("writer");
        if b.num_rows() > 0 {
            w.write(b).expect("write");
        }
        w.close().expect("close");
    }

    /// Read a written file back as one batch. Every fixture here is a handful of
    /// rows in a single row group, so more than one batch means the writer did
    /// something unexpected and the test should say so.
    fn read(path: &Path) -> RecordBatch {
        let f = File::open(path).expect("open");
        let mut batches: Vec<RecordBatch> = ParquetRecordBatchReaderBuilder::try_new(f)
            .expect("footer")
            .build()
            .expect("reader")
            .collect::<Result<_, _>>()
            .expect("batches");
        assert_eq!(batches.len(), 1, "fixture should be one batch");
        batches.pop().expect("batch")
    }

    #[test]
    fn adaptive_chunks_preserve_multifile_rows_order_and_empty_parts() {
        if !loaded() {
            return;
        }
        let scratch = Scratch::new();
        let parts = scratch.join("parts");
        std::fs::create_dir(&parts).expect("parts");
        write(
            &parts.join("00-empty.parquet"),
            &batch(vec![utf8("vin", &[]), i32s("year", &[])]),
        );
        let hondas = vec![Some(HONDA); 5_000];
        let honda_years = vec![Some(2003); 5_000];
        write(
            &parts.join("01-honda.parquet"),
            &batch(vec![utf8("vin", &hondas), i32s("year", &honda_years)]),
        );
        let fords = vec![Some(FORD); 5_000];
        let ford_years = vec![Some(2013); 5_000];
        write(
            &parts.join("02-ford.parquet"),
            &batch(vec![utf8("vin", &fords), i32s("year", &ford_years)]),
        );
        let named = ParquetOpts {
            vin: Some("vin".into()),
            year: Some("year".into()),
            ..opts(&[MAKE])
        };
        let chunks: Vec<_> = open_chunks_auto_at(&parts, named, 64 << 20, 0)
            .expect("open")
            .collect::<Result<_, _>>()
            .expect("decode");
        assert!(chunks.iter().all(|batch| batch.num_rows() <= 16_384));
        let vins: Vec<_> = chunks
            .iter()
            .flat_map(|batch| {
                batch
                    .column_by_name("vin")
                    .expect("vin")
                    .as_string::<i32>()
                    .iter()
            })
            .flatten()
            .collect();
        assert_eq!(vins.len(), 10_000);
        assert!(vins[..5_000].iter().all(|vin| *vin == HONDA));
        assert!(vins[5_000..].iter().all(|vin| *vin == FORD));
    }

    fn col_utf8(b: &RecordBatch, name: &str) -> Vec<Option<String>> {
        let a = b.column_by_name(name).expect(name).as_string::<i32>();
        (0..a.len())
            .map(|i| (!a.is_null(i)).then(|| a.value(i).to_string()))
            .collect()
    }

    fn col_i32(b: &RecordBatch, name: &str) -> Vec<Option<i32>> {
        let a = b
            .column_by_name(name)
            .expect(name)
            .as_primitive::<arrow_array::types::Int32Type>();
        (0..a.len())
            .map(|i| (!a.is_null(i)).then(|| a.value(i)))
            .collect()
    }

    fn col_i64(b: &RecordBatch, name: &str) -> Vec<Option<i64>> {
        let a = b
            .column_by_name(name)
            .expect(name)
            .as_primitive::<arrow_array::types::Int64Type>();
        (0..a.len())
            .map(|i| (!a.is_null(i)).then(|| a.value(i)))
            .collect()
    }

    fn col_f64(b: &RecordBatch, name: &str) -> Vec<Option<f64>> {
        let a = b
            .column_by_name(name)
            .expect(name)
            .as_primitive::<arrow_array::types::Float64Type>();
        (0..a.len())
            .map(|i| (!a.is_null(i)).then(|| a.value(i)))
            .collect()
    }

    /// `n` columns of values that are neither VIN-shaped nor year-shaped, so
    /// autodetect walks past them — the width a real export pads a VIN with.
    fn filler(n: usize, rows: usize) -> Vec<(Field, ArrayRef)> {
        (0..n)
            .map(|i| utf8(&format!("pad{i}"), &vec![Some("filler"); rows]))
            .collect()
    }

    /// How many of the input's columns the reader will actually decode.
    fn reader_width(path: &Path, opts: &ParquetOpts) -> usize {
        use arrow_array::RecordBatchReader;
        let metas = resolve_columns(Db::embedded(), &opts.columns).expect("columns");
        let st = FileState::open(
            path,
            opts,
            &metas,
            crate::now_micros(),
            opts.batch_size.max(1),
        )
        .expect("open");
        st.reader.schema().fields().len()
    }

    fn out_names(b: &RecordBatch) -> Vec<String> {
        b.schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    /// The message from a rejected open. Takes the whole `Result` because
    /// `ParquetChunkIter` is not `Debug`, so `unwrap_err` is unavailable.
    fn config_err(r: Result<ParquetChunkIter, ParquetError>) -> String {
        match r {
            Err(ParquetError::Config(m)) => m,
            Err(other) => panic!("expected a Config error, got {other:?}"),
            Ok(_) => panic!("expected the open to be refused"),
        }
    }

    #[test]
    fn round_trip_projects_typed_columns_and_keeps_row_order() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        write(
            &src,
            &batch(vec![
                utf8("vin", &[Some(HONDA), Some(MISS), None, Some(FORD)]),
                i32s("year", &[Some(2013), None, None, None]),
            ]),
        );

        let rows = decode_parquet_to_file(&src, &dst, opts(&[MAKE, CYLINDERS, DISPLACEMENT_L]))
            .expect("decode");
        assert_eq!(rows, 4);

        let out = read(&dst);
        assert_eq!(
            out.schema()
                .fields()
                .iter()
                .map(|f| (f.name().clone(), f.data_type().clone()))
                .collect::<Vec<_>>(),
            vec![
                ("vin".to_string(), DataType::Utf8),
                ("year".to_string(), DataType::Int32),
                ("decoded_model_year".to_string(), DataType::Int32),
                ("Make".to_string(), DataType::Utf8),
                ("Engine Number of Cylinders".to_string(), DataType::Int64),
                ("Displacement (L)".to_string(), DataType::Float64),
            ]
        );
        assert_eq!(out.num_rows(), 4);
        // Passthrough columns survive verbatim, nulls included.
        assert_eq!(
            col_utf8(&out, "vin"),
            vec![
                Some(HONDA.to_string()),
                Some(MISS.to_string()),
                None,
                Some(FORD.to_string())
            ]
        );
        assert_eq!(col_i32(&out, "year"), vec![Some(2013), None, None, None]);
        // Row 0's caller year (2013) overrides the VIN-derived 2003, and drops
        // its pattern match with it — that is what proves the hint got through.
        assert_eq!(
            col_i32(&out, "decoded_model_year"),
            vec![Some(2013), Some(2003), None, Some(2013)]
        );
        assert_eq!(
            col_utf8(&out, "Make"),
            vec![Some("HONDA".into()), None, None, Some("FORD".into())]
        );
        assert_eq!(
            col_i64(&out, "Engine Number of Cylinders"),
            vec![None, None, None, Some(6)]
        );
        let disp = col_f64(&out, "Displacement (L)");
        assert_eq!(disp[..3], [None, None, None]);
        assert!(
            (disp[3].expect("displacement") - 3.5).abs() < 1e-9,
            "displacement was {:?}",
            disp[3]
        );
    }

    #[test]
    fn the_vin_column_is_matched_by_name_case_insensitively() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        // Values that would never sniff as VIN-like: only the name can find it.
        write(&src, &batch(vec![utf8("VIN", &[Some(HONDA), Some("x")])]));
        assert_eq!(
            decode_parquet_to_file(&src, &dst, opts(&[MAKE])).expect("decode"),
            2
        );
        assert_eq!(col_utf8(&read(&dst), "Make")[0].as_deref(), Some("HONDA"));
    }

    #[test]
    fn an_unnamed_vin_column_is_sniffed_and_its_rows_still_emit() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        // No column called "vin" and no column called "year": both are found by
        // sniffing the first batch, which must then still be decoded — that
        // batch is the whole file here, so a dropped sniff batch means 0 rows.
        write(
            &src,
            &batch(vec![
                utf8("chassis_no", &[Some(HONDA), Some(FORD), Some(MISS)]),
                i32s("built", &[Some(2013), Some(2013), Some(2013)]),
                i32s("axles", &[Some(2), Some(2), Some(3)]),
            ]),
        );
        assert_eq!(
            decode_parquet_to_file(&src, &dst, opts(&[MAKE])).expect("decode"),
            3
        );
        let out = read(&dst);
        assert_eq!(
            out.schema().field(0).name(),
            "chassis_no",
            "the sniffed VIN column passes through under its own name"
        );
        assert_eq!(out.schema().field(1).name(), "built");
        // `axles` holds ints outside the model-year window, so it is not a
        // year candidate and the sniff stays unambiguous.
        assert_eq!(col_i32(&out, "decoded_model_year"), vec![Some(2013); 3]);
    }

    #[test]
    fn two_columns_named_vin_are_a_config_error() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(
            &src,
            &batch(vec![
                utf8("vin", &[Some(HONDA)]),
                utf8("VIN", &[Some(FORD)]),
            ]),
        );
        let err = config_err(open_chunks(&src, opts(&[MAKE])));
        assert!(err.contains("ambiguous VIN column"), "{err}");
    }

    #[test]
    fn two_sniffed_year_candidates_are_a_config_error() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(
            &src,
            &batch(vec![
                utf8("vin", &[Some(HONDA)]),
                i32s("built", &[Some(2013)]),
                i32s("sold", &[Some(2014)]),
            ]),
        );
        let err = config_err(open_chunks(&src, opts(&[MAKE])));
        assert!(err.contains("ambiguous caller-year column"), "{err}");
    }

    #[test]
    fn no_vin_like_column_is_a_config_error() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(&src, &batch(vec![utf8("notes", &[Some("hello"), None])]));
        let err = config_err(open_chunks(&src, opts(&[MAKE])));
        assert!(
            err.contains("could not autodetect a VIN-like column"),
            "{err}"
        );
    }

    #[test]
    fn explicit_column_names_skip_autodetect_entirely() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        // Two VIN-shaped columns and two year-shaped ones: autodetect would
        // refuse both, so naming them is the only way through.
        write(
            &src,
            &batch(vec![
                utf8("primary", &[Some(HONDA)]),
                utf8("secondary", &[Some(FORD)]),
                i32s("built", &[Some(2013)]),
                i32s("sold", &[Some(2014)]),
            ]),
        );
        let named = ParquetOpts {
            vin: Some("secondary".to_string()),
            year: Some("sold".to_string()),
            ..opts(&[MAKE])
        };
        assert_eq!(
            decode_parquet_to_file(&src, &dst, named).expect("decode"),
            1
        );
        let out = read(&dst);
        assert_eq!(col_utf8(&out, "Make"), vec![Some("FORD".to_string())]);
        assert_eq!(col_i32(&out, "decoded_model_year"), vec![Some(2014)]);

        let missing = ParquetOpts {
            vin: Some("nope".to_string()),
            ..opts(&[MAKE])
        };
        let err = config_err(open_chunks(&src, missing));
        assert!(err.contains("no column named \"nope\""), "{err}");
    }

    #[test]
    fn a_directory_is_read_as_one_stream_in_sorted_order() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let parts = dir.join("parts");
        std::fs::create_dir_all(&parts).unwrap();
        // Written out of order; the reader sorts by path.
        write(
            &parts.join("b.parquet"),
            &batch(vec![utf8("vin", &[Some(FORD)])]),
        );
        write(
            &parts.join("a.parquet"),
            &batch(vec![utf8("vin", &[Some(HONDA), Some(MISS)])]),
        );
        std::fs::write(parts.join("README.txt"), b"ignored").unwrap();

        let dst = dir.join("out.parquet");
        assert_eq!(
            decode_parquet_to_file(&parts, &dst, opts(&[MAKE])).expect("decode"),
            3
        );
        assert_eq!(
            col_utf8(&read(&dst), "Make"),
            vec![Some("HONDA".to_string()), None, Some("FORD".to_string())]
        );
    }

    #[test]
    fn an_empty_input_still_writes_a_valid_empty_parquet() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        write(&src, &batch(vec![utf8("vin", &[])]));

        assert_eq!(
            decode_parquet_to_file(&src, &dst, opts(&[MAKE])).expect("decode"),
            0
        );
        let f = File::open(&dst).unwrap();
        let schema = ParquetRecordBatchReaderBuilder::try_new(f)
            .expect("footer")
            .schema()
            .clone();
        assert_eq!(
            schema
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect::<Vec<_>>(),
            vec!["vin", "decoded_model_year", "Make"]
        );
    }

    #[test]
    fn an_empty_input_with_nothing_to_sniff_is_a_config_error() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(&src, &batch(vec![utf8("chassis_no", &[])]));
        let err = config_err(open_chunks(&src, opts(&[MAKE])));
        assert!(err.contains("from an empty file"), "{err}");
    }

    #[test]
    fn a_projected_label_colliding_with_a_passthrough_is_refused() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(&src, &batch(vec![utf8("Make", &[Some(HONDA)])]));
        let named = ParquetOpts {
            vin: Some("Make".to_string()),
            ..opts(&[MAKE])
        };
        let err = config_err(open_chunks(&src, named));
        assert!(err.contains("collides with a passthrough column"), "{err}");

        let src2 = dir.join("in2.parquet");
        write(
            &src2,
            &batch(vec![
                utf8("vin", &[Some(HONDA)]),
                i32s("decoded_model_year", &[Some(2013)]),
            ]),
        );
        let named = ParquetOpts {
            year: Some("decoded_model_year".to_string()),
            ..opts(&[MAKE])
        };
        let err = config_err(open_chunks(&src2, named));
        assert!(err.contains("already has a column named"), "{err}");
    }

    #[test]
    fn bad_element_ids_are_rejected_before_any_io() {
        if !loaded() {
            return;
        }
        let err = config_err(open_chunks(Path::new("/nonexistent"), opts(&[999_999])));
        assert!(err.contains("unknown element_id"), "{err}");
    }

    #[test]
    fn a_missing_source_is_an_io_error() {
        if !loaded() {
            return;
        }
        match open_chunks(Path::new("/nonexistent/x.parquet"), opts(&[MAKE])) {
            Err(ParquetError::Io(m)) => assert!(m.contains("not found"), "{m}"),
            Err(other) => panic!("expected an Io error, got {other:?}"),
            Ok(_) => panic!("expected the open to be refused"),
        }
    }

    #[test]
    fn a_destination_that_is_the_source_is_refused() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(&src, &batch(vec![utf8("vin", &[Some(HONDA)])]));

        match decode_parquet_to_file(&src, &src, opts(&[MAKE])) {
            Err(ParquetError::Config(m)) => {
                assert!(m.contains("is the source being decoded"), "{m}")
            }
            other => panic!("expected the write to be refused, got {other:?}"),
        }
        // Refused before opening the writer, so the input is still readable.
        assert_eq!(read(&src).num_rows(), 1);

        let parts = dir.join("parts");
        std::fs::create_dir_all(&parts).unwrap();
        write(
            &parts.join("a.parquet"),
            &batch(vec![utf8("vin", &[Some(HONDA)])]),
        );
        match decode_parquet_to_file(&parts, &parts.join("out.parquet"), opts(&[MAKE])) {
            Err(ParquetError::Config(m)) => {
                assert!(m.contains("inside the source directory"), "{m}")
            }
            other => panic!("expected the write to be refused, got {other:?}"),
        }
    }

    #[test]
    fn failed_write_preserves_existing_destination_and_removes_temporary_file() {
        let dir = Scratch::new();
        let dst = dir.join("out.parquet");
        std::fs::write(&dst, b"previous output").expect("seed destination");
        let input = batch(vec![utf8("vin", &[Some(HONDA)])]);
        let schema = input.schema();
        let batches = vec![
            Ok(input),
            Err(ParquetError::Io("injected reader failure".to_string())),
        ];

        let err = write_parquet(batches.into_iter(), schema, &dst, 1).unwrap_err();
        assert!(err.to_string().contains("injected reader failure"), "{err}");
        assert_eq!(std::fs::read(&dst).unwrap(), b"previous output");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn failed_write_leaves_no_destination_when_none_existed() {
        let dir = Scratch::new();
        let dst = dir.join("out.parquet");
        let input = batch(vec![utf8("vin", &[Some(HONDA)])]);
        let schema = input.schema();
        let batches = vec![
            Ok(input),
            Err(ParquetError::Io("injected reader failure".to_string())),
        ];

        assert!(write_parquet(batches.into_iter(), schema, &dst, 1).is_err());
        assert!(!dst.exists());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    #[test]
    fn successful_write_atomically_replaces_existing_destination() {
        let dir = Scratch::new();
        let dst = dir.join("out.parquet");
        std::fs::write(&dst, b"previous output").expect("seed destination");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let input = batch(vec![utf8("vin", &[Some(HONDA), Some(FORD)])]);
        let schema = input.schema();

        assert_eq!(
            write_parquet(vec![Ok(input)].into_iter(), schema, &dst, 2).unwrap(),
            2
        );
        assert_eq!(read(&dst).num_rows(), 2);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[test]
    fn rename_failure_preserves_destination_and_removes_temporary_file() {
        let dir = Scratch::new();
        let dst = dir.join("existing-directory");
        std::fs::create_dir(&dst).unwrap();
        let input = batch(vec![utf8("vin", &[Some(HONDA)])]);
        let schema = input.schema();

        let err = write_parquet(vec![Ok(input)].into_iter(), schema, &dst, 1).unwrap_err();
        assert!(
            err.to_string().contains("installing completed parquet"),
            "{err}"
        );
        assert!(dst.is_dir());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn a_hardlink_destination_does_not_modify_the_source_inode() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        write(&src, &batch(vec![utf8("vin", &[Some(HONDA)])]));
        std::fs::hard_link(&src, &dst).expect("hard link");

        assert_eq!(
            decode_parquet_to_file(&src, &dst, opts(&[MAKE])).unwrap(),
            1
        );
        assert_eq!(col_utf8(&read(&src), "vin"), [Some(HONDA.to_string())]);
        assert_eq!(col_utf8(&read(&dst), "Make"), [Some("HONDA".to_string())]);
    }

    #[test]
    fn files_resolving_different_columns_are_refused() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let parts = dir.join("parts");
        std::fs::create_dir_all(&parts).unwrap();
        // `b` carries a year column `a` lacks, so the two files project different
        // output shapes — zipped positionally that would write a passthrough year
        // into `decoded_model_year`.
        write(
            &parts.join("a.parquet"),
            &batch(vec![utf8("vin", &[Some(HONDA)])]),
        );
        write(
            &parts.join("b.parquet"),
            &batch(vec![
                utf8("vin", &[Some(HONDA)]),
                i32s("year", &[Some(2013)]),
            ]),
        );

        let mut iter = open_chunks(&parts, opts(&[MAKE])).expect("open");
        assert_eq!(
            iter.next_chunk()
                .expect("first file")
                .expect("a chunk")
                .num_rows(),
            1
        );
        match iter.next_chunk() {
            Err(ParquetError::Config(m)) => {
                assert!(m.contains("b.parquet"), "the offending file is named: {m}");
                assert!(m.contains("passes through"), "{m}");
            }
            other => panic!("expected the second file to be refused, got {other:?}"),
        }

        let dst = dir.join("out.parquet");
        match decode_parquet_to_file(&parts, &dst, opts(&[MAKE])) {
            Err(ParquetError::Config(m)) => assert!(m.contains("b.parquet"), "{m}"),
            other => panic!("expected the decode to be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_chunk_stops_the_iterator_rather_than_skipping_a_file() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let parts = dir.join("parts");
        std::fs::create_dir_all(&parts).unwrap();
        write(
            &parts.join("a.parquet"),
            &batch(vec![utf8("vin", &[Some(HONDA)])]),
        );
        std::fs::write(parts.join("b.parquet"), b"not a parquet file").unwrap();
        write(
            &parts.join("c.parquet"),
            &batch(vec![utf8("vin", &[Some(FORD)])]),
        );

        let mut iter = open_chunks(&parts, opts(&[MAKE])).expect("open");
        assert_eq!(
            iter.next_chunk()
                .expect("first file")
                .expect("a chunk")
                .num_rows(),
            1
        );
        assert!(
            iter.next_chunk().is_err(),
            "the unreadable file must be reported"
        );
        // `c` is never reached: resuming would hand back rows with a silent hole
        // where `b` should have been.
        assert!(iter.next_chunk().expect("fused").is_none());
    }

    #[test]
    fn a_dictionary_encoded_vin_column_is_autodetected() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        // What pandas writes for a categorical column. The name is unknown, so
        // only the sniffer can find it — and the sniffer skips non-text columns.
        let dict: arrow_array::DictionaryArray<arrow_array::types::Int32Type> =
            [HONDA, FORD, HONDA].into_iter().collect();
        write(
            &src,
            &batch(vec![(
                Field::new("chassis_no", dict.data_type().clone(), true),
                Arc::new(dict) as ArrayRef,
            )]),
        );
        assert_eq!(
            decode_parquet_to_file(&src, &dst, opts(&[MAKE])).expect("decode"),
            3
        );
        assert_eq!(
            col_utf8(&read(&dst), "Make"),
            vec![
                Some("HONDA".to_string()),
                Some("FORD".to_string()),
                Some("HONDA".to_string())
            ]
        );
    }

    #[test]
    fn a_vin_column_that_is_not_text_is_refused() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(
            &src,
            &batch(vec![utf8("vin", &[Some(HONDA)]), i32s("axles", &[Some(2)])]),
        );
        let named = ParquetOpts {
            vin: Some("axles".to_string()),
            ..opts(&[MAKE])
        };
        let err = config_err(open_chunks(&src, named));
        assert!(err.contains("holds Int32, not text"), "{err}");
    }

    #[test]
    fn one_column_cannot_be_both_the_vin_and_the_year() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        write(&src, &batch(vec![utf8("c", &[Some(HONDA)])]));
        let named = ParquetOpts {
            vin: Some("c".to_string()),
            year: Some("c".to_string()),
            ..opts(&[MAKE])
        };
        let err = config_err(open_chunks(&src, named));
        assert!(err.contains("both the VIN and the caller-year"), "{err}");
    }

    #[test]
    fn chunks_stream_at_batch_size_and_cover_every_row() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let vins: Vec<Option<&str>> = vec![Some(HONDA); 5];
        write(&src, &batch(vec![utf8("vin", &vins)]));

        let chunked = ParquetOpts {
            batch_size: 2,
            ..opts(&[MAKE])
        };
        let iter = open_chunks(&src, chunked).expect("open");
        assert!(
            iter.out_schema.is_some(),
            "schema known before the first chunk"
        );
        let sizes: Vec<usize> = iter.map(|b| b.expect("chunk").num_rows()).collect();
        assert_eq!(sizes, vec![2, 2, 1]);
    }

    #[test]
    fn a_wide_file_decodes_only_the_named_columns() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        let mut cols = filler(30, 2);
        cols.insert(4, utf8("chassis_no", &[Some(HONDA), Some(FORD)]));
        cols.insert(25, i32s("built", &[Some(2013), None]));
        write(&src, &batch(cols));

        let named = ParquetOpts {
            vin: Some("chassis_no".to_string()),
            year: Some("built".to_string()),
            ..opts(&[MAKE])
        };
        assert_eq!(reader_width(&src, &named), 2, "32 columns in, 2 read");
        assert_eq!(
            decode_parquet_to_file(&src, &dst, named).expect("decode"),
            2
        );

        let out = read(&dst);
        assert_eq!(
            out_names(&out),
            ["chassis_no", "built", "decoded_model_year", "Make"].map(String::from)
        );
        assert_eq!(
            col_utf8(&out, "chassis_no"),
            vec![Some(HONDA.to_string()), Some(FORD.to_string())]
        );
        assert_eq!(col_i32(&out, "built"), vec![Some(2013), None]);
        assert_eq!(
            col_i32(&out, "decoded_model_year"),
            vec![Some(2013), Some(2013)]
        );
        assert_eq!(
            col_utf8(&out, "Make"),
            vec![Some("HONDA".to_string()), Some("FORD".to_string())]
        );
    }

    #[test]
    fn a_wide_file_autodetects_across_the_filler_columns() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        // Neither name resolves, so both columns come out of the sniffer — which
        // reads the full width, then hands the decode a projected reader.
        let mut cols = filler(30, 2);
        cols.insert(4, utf8("chassis_no", &[Some(HONDA), Some(FORD)]));
        cols.insert(25, i32s("built", &[Some(2013), Some(2014)]));
        write(&src, &batch(cols));

        assert_eq!(
            reader_width(&src, &opts(&[MAKE])),
            2,
            "32 columns in, 2 read"
        );
        assert_eq!(
            decode_parquet_to_file(&src, &dst, opts(&[MAKE])).expect("decode"),
            2
        );

        let out = read(&dst);
        assert_eq!(
            out_names(&out),
            ["chassis_no", "built", "decoded_model_year", "Make"].map(String::from)
        );
        assert_eq!(
            col_i32(&out, "decoded_model_year"),
            vec![Some(2013), Some(2014)]
        );
        assert_eq!(
            col_utf8(&out, "Make"),
            vec![Some("HONDA".to_string()), Some("FORD".to_string())]
        );
    }

    #[test]
    fn a_year_column_ahead_of_the_vin_column_keeps_both_straight() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        // File order is year-then-VIN, so the projection renumbers them the other
        // way round from the usual fixture.
        let mut cols = filler(30, 2);
        cols.insert(2, i32s("built", &[Some(2013), Some(2014)]));
        cols.insert(28, utf8("chassis_no", &[Some(HONDA), Some(FORD)]));
        write(&src, &batch(cols));

        let named = ParquetOpts {
            vin: Some("chassis_no".to_string()),
            year: Some("built".to_string()),
            ..opts(&[MAKE])
        };
        assert_eq!(
            decode_parquet_to_file(&src, &dst, named).expect("decode"),
            2
        );

        let out = read(&dst);
        assert_eq!(
            col_utf8(&out, "chassis_no"),
            vec![Some(HONDA.to_string()), Some(FORD.to_string())]
        );
        assert_eq!(col_i32(&out, "built"), vec![Some(2013), Some(2014)]);
        assert_eq!(
            col_utf8(&out, "Make"),
            vec![Some("HONDA".to_string()), Some("FORD".to_string())]
        );
    }

    #[test]
    fn a_wide_file_without_a_year_column_reads_the_vin_alone() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let dst = dir.join("out.parquet");
        let mut cols = filler(30, 3);
        cols.insert(
            17,
            utf8("chassis_no", &[Some(HONDA), Some(MISS), Some(FORD)]),
        );
        write(&src, &batch(cols));

        assert_eq!(reader_width(&src, &opts(&[MAKE])), 1, "no year to read");
        assert_eq!(
            decode_parquet_to_file(&src, &dst, opts(&[MAKE])).expect("decode"),
            3
        );

        let out = read(&dst);
        assert_eq!(
            out_names(&out),
            ["chassis_no", "decoded_model_year", "Make"].map(String::from)
        );
        assert_eq!(
            col_i32(&out, "decoded_model_year"),
            vec![Some(2003), Some(2003), Some(2013)]
        );
        assert_eq!(
            col_utf8(&out, "Make"),
            vec![Some("HONDA".to_string()), None, Some("FORD".to_string())]
        );
    }

    const NOW: i64 = 1_788_220_800_000_000;

    fn read_all(path: &Path) -> RecordBatch {
        let f = File::open(path).expect("open");
        let reader = ParquetRecordBatchReaderBuilder::try_new(f)
            .expect("footer")
            .build()
            .expect("reader");
        let schema = reader.schema();
        let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().expect("batches");
        arrow_select::concat::concat_batches(&schema, &batches).expect("concat")
    }

    /// `src` decoded the wide way, at [`NOW`], into `dst`.
    fn wide(src: &Path, dst: &Path, opts: ParquetOpts) -> RecordBatch {
        let iter = open_chunks_at(src, opts, NOW).expect("open");
        let schema = iter.out_schema.clone().expect("schema");
        write_parquet(iter, schema, dst, 65_536).expect("wide decode");
        read_all(dst)
    }

    /// Join the compact pair in `dir` and check it is `wide`, row for row and
    /// column for column; returns the vehicle rows.
    fn assert_joins_to(dir: &Path, wide: &RecordBatch) -> usize {
        use arrow_array::types::UInt64Type;
        let vins = read_all(&dir.join(COMPACT_VINS));
        let vehicles = read_all(&dir.join(COMPACT_VEHICLES));
        assert_eq!(vins.num_rows(), wide.num_rows());
        assert_eq!(vins.num_columns(), 2);
        assert_eq!(vins.schema().field(0), wide.schema().field(0));
        assert_eq!(vins.column(0), wide.column(0), "the VINs as given");
        let ids = vehicles
            .column_by_name(VEHICLE_ID)
            .expect("vehicle ids")
            .as_primitive::<UInt64Type>();
        let mut row_of = std::collections::HashMap::new();
        for (row, &id) in ids.values().iter().enumerate() {
            assert!(row_of.insert(id, row as u64).is_none(), "id {id} twice");
        }
        let vin_ids = vins.column(1).as_primitive::<UInt64Type>();
        let rows: arrow_array::UInt64Array = vin_ids.values().iter().map(|id| row_of[id]).collect();
        let referenced: std::collections::HashSet<_> = vin_ids.values().iter().collect();
        assert_eq!(referenced.len(), row_of.len(), "every vehicle row is used");
        assert_eq!(vehicles.num_columns(), wide.num_columns());
        assert_eq!(vehicles.schema().metadata(), wide.schema().metadata());
        for (column, field) in wide.columns().iter().zip(wide.schema().fields()).skip(1) {
            let (at, vehicle_field) = vehicles
                .schema()
                .column_with_name(field.name())
                .map(|(at, f)| (at, f.clone()))
                .unwrap_or_else(|| panic!("{}", field.name()));
            assert_eq!(&vehicle_field, field.as_ref());
            let joined = arrow_select::take::take(vehicles.column(at), &rows, None).expect("take");
            assert_eq!(&joined, column, "{}", field.name());
        }
        vehicles.num_rows()
    }

    /// `vin` with position 9 set to its check digit, when it can have one.
    fn with_check_digit(vin: &str) -> Option<String> {
        let digit = crate::checkdigit::check_digit_with_flag(vin, false).filter(|&d| d != '?')?;
        let mut bytes = vin.as_bytes().to_vec();
        bytes[8] = digit as u8;
        String::from_utf8(bytes).ok()
    }

    /// Clean VINs, the cover's and two common ones, each with serial siblings
    /// (which share its vehicle), a wrong check digit, lowercase and padded,
    /// cut short, and raw; then garbage and nulls; all of it twice, so later
    /// rows find vehicles earlier ones decoded. A seed's rows share a caller
    /// year, cycling through none, plausible ones and one out of range.
    fn mixed_corpus() -> (Vec<Option<String>>, Vec<Option<i32>>) {
        let (mut vins, mut years) = (Vec::new(), Vec::new());
        let db = Db::embedded();
        let common = [
            HONDA,
            FORD,
            "5YJ3E1EA7KF317000",
            "5YJSA1E26HF000337",
            "5UXWX7C5XBA123456",
            "3C6TRVAG6JE100000",
            "1M8GDM9AXKP042788",
            "JH4KA8260MC000000",
            "1FMAA50A91A111111",
            "4P1CT02S91A001458",
        ];
        let mut seeds: Vec<_> = common.map(str::to_owned).into();
        let filter = crate::Filter::default();
        let year = crate::current_year_at(NOW);
        seeds.extend(crate::generate(db, 150, 7, &filter, NOW, year));
        seeds.extend(db.cover().into_iter().filter(|s| s.len() == 17).take(150));
        for (k, seed) in seeds.into_iter().enumerate() {
            let start = vins.len();
            vins.push(Some(seed.clone()));
            if let Some(clean) = with_check_digit(&seed) {
                let siblings = if k < common.len() { 60 } else { 4 };
                for step in 1..=siblings {
                    let mut sibling = clean.clone().into_bytes();
                    let serial = (10_000 + step * 7_919 % 90_000).to_string();
                    for (c, digit) in sibling[12..].iter_mut().zip(serial.bytes()) {
                        if c.is_ascii_digit() {
                            *c = digit;
                        }
                    }
                    let sibling = String::from_utf8(sibling).expect("ASCII");
                    vins.extend(with_check_digit(&sibling).map(Some));
                }
                let mut wrong = clean.clone().into_bytes();
                wrong[8] = if wrong[8] == b'1' { b'2' } else { b'1' };
                vins.push(Some(String::from_utf8(wrong).expect("ASCII")));
                vins.push(Some(format!("  {}\t", clean.to_ascii_lowercase())));
                vins.push(Some(clean[..11].to_string()));
                vins.push(Some(clean));
            }
            let year = [None, Some(2003), None, None, Some(2015), Some(1979)][k % 6];
            years.resize(vins.len(), year);
            debug_assert_eq!(years.len() - start, vins.len() - start);
        }
        for garbage in [
            None,
            Some(""),
            Some("INVALID"),
            Some("1HGCM82633A00435Ł"),
            Some("1HGCM82633A0043521"),
        ] {
            vins.push(garbage.map(str::to_owned));
            years.push(None);
        }
        (vins.iter().chain(&vins).cloned().collect(), years.repeat(2))
    }

    fn write_corpus(path: &Path, vins: &[Option<String>], years: Option<&[Option<i32>]>) {
        let vins: Vec<Option<&str>> = vins.iter().map(Option::as_deref).collect();
        let mut cols = vec![utf8("vin", &vins)];
        cols.extend(years.map(|years| i32s("year", years)));
        write(path, &batch(cols));
    }

    #[test]
    fn the_compact_pair_joins_back_to_the_wide_output() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let src = dir.join("in.parquet");
        let (vins, years) = mixed_corpus();
        write_corpus(&src, &vins, Some(&years));
        let all = ParquetOpts {
            columns: crate::all_public_ids(Db::embedded())
                .into_iter()
                .map(ColumnSpec::Id)
                .collect(),
            ..ParquetOpts::default()
        };
        let expected = wide(&src, &dir.join("wide.parquet"), all.clone());
        // One chunk, then short ones: several streams, the last partial.
        for chunk_rows in [COMPACT_CHUNK_ROWS, 1_000] {
            let out = dir.join(&format!("out-{chunk_rows}"));
            let rows = compact(&src, &out, all.clone(), NOW, chunk_rows).expect("compact");
            assert_eq!(rows.vins, vins.len());
            assert_eq!(assert_joins_to(&out, &expected), rows.vehicles);
            assert!(rows.vehicles + 1_000 < rows.vins, "repeats share: {rows:?}");
        }

        // Ids name the column the projection asks for, under either naming.
        let named = ParquetOpts {
            names: ColumnNames::Id,
            ..opts(&[MAKE, CYLINDERS, DISPLACEMENT_L])
        };
        let expected = wide(&src, &dir.join("wide-ids.parquet"), named.clone());
        decode_compact_at(&src, &dir.join("ids"), named, NOW).expect("compact");
        assert_joins_to(&dir.join("ids"), &expected);
    }

    #[test]
    fn a_text_file_is_one_vin_per_line() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let lines = [
            HONDA,
            "",
            " 1hgcm82633a004352",
            FORD,
            MISS,
            HONDA,
            "not a vin",
            FORD,
        ];
        let src = dir.join("in.txt");
        std::fs::write(&src, format!("{}\r\n{}", lines[..7].join("\n"), lines[7])).unwrap();
        let parquet = dir.join("in.parquet");
        let vins: Vec<_> = lines.iter().map(|line| Some(line.to_string())).collect();
        write_corpus(&parquet, &vins, None);
        let expected = wide(
            &parquet,
            &dir.join("wide.parquet"),
            opts(&[MAKE, CYLINDERS]),
        );
        // Three-line chunks split the file mid-buffer, and the last is short.
        let rows =
            compact(&src, &dir.join("out"), opts(&[MAKE, CYLINDERS]), NOW, 3).expect("compact");
        assert_eq!(rows.vins, lines.len());
        assert_joins_to(&dir.join("out"), &expected);

        // A trailing newline ends the last line; it does not start another.
        std::fs::write(&src, format!("{HONDA}\n{FORD}\n")).unwrap();
        let rows = decode_compact_at(&src, &dir.join("out"), opts(&[MAKE]), NOW).expect("compact");
        assert_eq!(rows.vins, 2);
        // An empty file is two empty files with the full schemas.
        std::fs::write(&src, "").unwrap();
        let rows = decode_compact_at(&src, &dir.join("out"), opts(&[MAKE]), NOW).expect("compact");
        assert_eq!(
            rows,
            CompactRows {
                vins: 0,
                vehicles: 0
            }
        );
        assert_eq!(
            out_names(&read_all(&dir.join("out").join(COMPACT_VINS))),
            ["vin", VEHICLE_ID]
        );
        assert_eq!(
            out_names(&read_all(&dir.join("out").join(COMPACT_VEHICLES))),
            [VEHICLE_ID, "decoded_model_year", "Make"]
        );
    }

    #[test]
    fn a_failed_compact_decode_replaces_neither_file() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let out = dir.join("out");
        let src = dir.join("in.txt");
        std::fs::write(&src, format!("{HONDA}\n{FORD}\n")).unwrap();
        decode_compact_at(&src, &out, opts(&[MAKE]), NOW).expect("compact");
        let before = |name: &str| std::fs::read(out.join(name)).unwrap();
        let (vins, vehicles) = (before(COMPACT_VINS), before(COMPACT_VEHICLES));
        // The second chunk holds a line that is not UTF-8.
        std::fs::write(
            &src,
            [HONDA.as_bytes(), b"\n", FORD.as_bytes(), b"\n\xff\n"].concat(),
        )
        .unwrap();
        match compact(&src, &out, opts(&[MAKE]), NOW, 2) {
            Err(ParquetError::Io(m)) => assert!(m.contains("line 3 is not UTF-8"), "{m}"),
            other => panic!("expected the bad line to fail the decode, got {other:?}"),
        }
        assert_eq!(before(COMPACT_VINS), vins);
        assert_eq!(before(COMPACT_VEHICLES), vehicles);
        assert_eq!(
            std::fs::read_dir(&out).unwrap().count(),
            2,
            "no temp files left"
        );
    }

    #[test]
    fn compact_outputs_stay_out_of_the_source_and_off_its_columns() {
        if !loaded() {
            return;
        }
        let dir = Scratch::new();
        let parts = dir.join("parts");
        std::fs::create_dir_all(&parts).unwrap();
        write(
            &parts.join("a.parquet"),
            &batch(vec![utf8("vin", &[Some(HONDA)])]),
        );
        match decode_compact_at(&parts, &parts.join("out"), opts(&[MAKE]), NOW) {
            Err(ParquetError::Config(m)) => assert!(m.contains("inside the source"), "{m}"),
            other => panic!("expected the destination to be refused, got {other:?}"),
        }
        let src = dir.join("in.parquet");
        write(
            &src,
            &batch(vec![
                utf8("vin", &[Some(HONDA)]),
                i32s(VEHICLE_ID, &[Some(2003)]),
            ]),
        );
        let named = ParquetOpts {
            year: Some(VEHICLE_ID.to_string()),
            ..opts(&[MAKE])
        };
        match decode_compact_at(&src, &dir.join("out"), named, NOW) {
            Err(ParquetError::Config(m)) => assert!(m.contains("collides"), "{m}"),
            other => panic!("expected the column to be refused, got {other:?}"),
        }
        match decode_compact_at(
            &dir.join("missing.txt"),
            &dir.join("out"),
            opts(&[MAKE]),
            NOW,
        ) {
            Err(ParquetError::Io(m)) => assert!(m.contains("not found"), "{m}"),
            other => panic!("expected a missing source to fail, got {other:?}"),
        }
    }
}
