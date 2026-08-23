//! Parquet-to-parquet dataset decode: read row groups, decode + project each
//! chunk through [`crate::decode_batch_ids`], write projected parquet.
//!
//! Memory is O(chunk), never O(file): the reader streams record batches at
//! `batch_size` rows, and the writer emits roughly one row group per chunk. It
//! is also flat in the input's width — the reader is projected down to the VIN
//! and caller-year columns, so a 500-column source costs what a 2-column one
//! does. No row of the output is materialized as a Python object on this path —
//! the pyo3 layer hands back either a row count or lazily-iterated column dicts.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    cast::AsArray, Array, ArrayRef, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
};
use arrow_cast::cast;
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::{ArrowWriter, ProjectionMask};
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::db::Db;
use crate::ids::{decode_batch_ids, resolve_ids, ColumnValues, IdMeta, IdsDType};

/// Caller-tunable knobs for a dataset job.
#[derive(Debug, Clone)]
pub struct ParquetOpts {
    /// Input VIN column name; `None` → autodetect.
    pub vin: Option<String>,
    /// Input caller-year column name; `None` → autodetect (absence is fine).
    pub year: Option<String>,
    /// Element ids to project; validated against the archive by [`open_chunks`].
    pub ids: Vec<i32>,
    /// Rows per chunk — memory, not throughput.
    pub batch_size: usize,
    /// How many leading rows to sniff when autodetecting columns.
    pub sample_rows: usize,
}

impl Default for ParquetOpts {
    fn default() -> Self {
        ParquetOpts {
            vin: None,
            year: None,
            ids: Vec::new(),
            batch_size: 65_536,
            sample_rows: 100,
        }
    }
}

/// `Io` covers unreadable files/footers/row groups; `Config` is a caller
/// mistake (missing/ambiguous columns, bad element ids) worth a `ValueError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParquetError {
    Io(String),
    Config(String),
}

impl std::fmt::Display for ParquetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParquetError::Io(m) | ParquetError::Config(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ParquetError {}

/// Year-column candidates tried by name before sniffing (case-insensitive) —
/// the common names across NHTSA, fleet-telemetry, and registration exports.
const YEAR_NAMES: [&str; 5] = [
    "year",
    "model_year",
    "model_yr_num",
    "veh_mfg_yr",
    "sf_model_year",
];

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

/// Text, including the dictionary encoding pandas gives a categorical column —
/// the cast in [`transform`] flattens it either way, so autodetect has no reason
/// to skip it.
fn is_stringish(dt: &DataType) -> bool {
    match dt {
        DataType::Utf8
        | DataType::LargeUtf8
        | DataType::Utf8View
        | DataType::Binary
        | DataType::LargeBinary
        | DataType::BinaryView => true,
        DataType::Dictionary(_, inner) => is_stringish(inner),
        _ => false,
    }
}

fn is_intish(dt: &DataType) -> bool {
    match dt {
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => true,
        DataType::Dictionary(_, inner) => is_intish(inner),
        _ => false,
    }
}

/// Ratio test: ≥90% of the non-null sampled values pass `pred`, at least one.
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
fn sniff_year_candidates(batch: &RecordBatch, sample: usize) -> Vec<usize> {
    let lo = 1980i64;
    let hi = i64::from(crate::ids::year_upper_bound());
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

/// Per-file reader state: which input columns feed the kernel, and the fixed
/// output schema every chunk from this file produces. `vin_idx`/`year_idx`
/// index the *projected* reader, not the file.
struct FileState {
    reader: parquet::arrow::arrow_reader::ParquetRecordBatchReader,
    vin_idx: usize,
    year_idx: Option<usize>,
    out_schema: SchemaRef,
}

impl FileState {
    /// Open one file: footer-based column resolution (names first, then a sniff
    /// over the leading rows), then a streaming reader projected down to the one
    /// or two columns the decode reads.
    fn open(path: &Path, opts: &ParquetOpts, metas: &[IdMeta]) -> Result<Self, ParquetError> {
        let file =
            File::open(path).map_err(|e| ParquetError::Io(format!("{}: {e}", path.display())))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
            ParquetError::Io(format!("{}: reading parquet footer: {e}", path.display()))
        })?;
        let schema = builder.schema().clone();

        // The sniff corpus, read at most once per file and only if a name fails
        // to resolve: outer `None` = not read yet, inner `None` = empty file.
        let mut sample: Option<Option<RecordBatch>> = None;

        let vin_idx = match &opts.vin {
            Some(name) => col_index(&schema, name)
                .ok_or_else(|| ParquetError::Config(format!("no column named {name:?}")))?,
            None => {
                let named = indices(&schema, |n| n.eq_ignore_ascii_case("vin"));
                match named.len() {
                    1 => named[0],
                    0 => {
                        let b = peek_first(&mut sample, path, opts)?.ok_or_else(|| {
                            ParquetError::Config(format!(
                                "{}: cannot autodetect the VIN column from an empty file — \
                                 name the column explicitly",
                                path.display()
                            ))
                        })?;
                        let cands = sniff_vin_candidates(b, opts.sample_rows);
                        one_candidate(&cands, &schema, "VIN-like")?
                    }
                    _ => {
                        return Err(ParquetError::Config(format!(
                            "ambiguous VIN column: {} all match by name",
                            named
                                .iter()
                                .map(|&i| schema.field(i).name().clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )))
                    }
                }
            }
        };

        let year_idx = match &opts.year {
            Some(name) => Some(
                col_index(&schema, name)
                    .ok_or_else(|| ParquetError::Config(format!("no column named {name:?}")))?,
            ),
            None => {
                let mut named = Vec::new();
                for cand in YEAR_NAMES {
                    named.extend(indices(&schema, |n| n.eq_ignore_ascii_case(cand)));
                }
                match named.len() {
                    1 => Some(named[0]),
                    0 => {
                        // No year column is fine — those rows just decode without
                        // the caller hint, so an empty file is not fatal here the
                        // way a missing VIN column is. Only ambiguity is a mistake.
                        let cands = match peek_first(&mut sample, path, opts)? {
                            Some(b) => sniff_year_candidates(b, opts.sample_rows),
                            None => Vec::new(),
                        };
                        if cands.len() > 1 {
                            return Err(ParquetError::Config(format!(
                                "ambiguous caller-year column: {}",
                                cands
                                    .iter()
                                    .map(|&i| schema.field(i).name().clone())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )));
                        }
                        cands.first().copied()
                    }
                    _ => {
                        return Err(ParquetError::Config(format!(
                            "ambiguous caller-year column: {}",
                            named
                                .iter()
                                .map(|&i| schema.field(i).name().clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )))
                    }
                }
            }
        };

        // A named VIN column that holds numbers would be cast to text and decoded
        // as garbage — silently, since an undecodable VIN is a null row by
        // design. Autodetect can only pick text, so this catches `vin=`.
        let vin_type = schema.field(vin_idx).data_type();
        if !is_stringish(vin_type) {
            return Err(ParquetError::Config(format!(
                "VIN column {:?} holds {vin_type}, not text",
                schema.field(vin_idx).name()
            )));
        }
        // One column cannot be both the VIN and the caller year: the output would
        // carry its name twice, with the second write winning.
        if year_idx == Some(vin_idx) {
            return Err(ParquetError::Config(format!(
                "column {:?} was named as both the VIN and the caller-year column",
                schema.field(vin_idx).name()
            )));
        }

        let out_schema = build_out_schema(&schema, vin_idx, year_idx, metas)?;

        // Nothing downstream of here reads any other column, so masking them out
        // keeps a chunk's cost off the input's width. A projected batch holds
        // only the kept columns, still in file order — the indices move with
        // them.
        let mut keep = vec![vin_idx];
        keep.extend(year_idx);
        keep.sort_unstable();
        let projected = |i: usize| keep.iter().position(|&k| k == i).expect("a kept column");
        let mask = ProjectionMask::roots(builder.parquet_schema(), keep.iter().copied());
        let reader = builder
            .with_projection(mask)
            .with_batch_size(opts.batch_size.max(1))
            .build()
            .map_err(|e| ParquetError::Io(format!("{}: {e}", path.display())))?;

        Ok(FileState {
            reader,
            vin_idx: projected(vin_idx),
            year_idx: year_idx.map(projected),
            out_schema,
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

fn col_index(schema: &SchemaRef, name: &str) -> Option<usize> {
    schema.fields().iter().position(|f| f.name() == name)
}

fn indices(schema: &SchemaRef, pred: impl Fn(&str) -> bool) -> Vec<usize> {
    schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, f)| pred(f.name()))
        .map(|(i, _)| i)
        .collect()
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
            many.iter()
                .map(|&i| schema.field(i).name().clone())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The output schema: passthrough vin (+ year), decoded_model_year, then one
/// typed field per requested id. A projection label colliding with a
/// passthrough name would silently merge two different things — refuse.
fn build_out_schema(
    in_schema: &SchemaRef,
    vin_idx: usize,
    year_idx: Option<usize>,
    metas: &[IdMeta],
) -> Result<SchemaRef, ParquetError> {
    let mut fields: Vec<Field> = Vec::with_capacity(metas.len() + 3);
    fields.push(Field::new(
        in_schema.field(vin_idx).name().clone(),
        DataType::Utf8,
        true,
    ));
    let mut taken: Vec<String> = vec![in_schema.field(vin_idx).name().clone()];
    if let Some(yi) = year_idx {
        let name = in_schema.field(yi).name().clone();
        taken.push(name.clone());
        fields.push(Field::new(name, DataType::Int32, true));
    }
    const DECODED_YEAR: &str = "decoded_model_year";
    if taken.iter().any(|t| t == DECODED_YEAR) {
        return Err(ParquetError::Config(format!(
            "input already has a column named {DECODED_YEAR}; remove or rename it"
        )));
    }
    taken.push(DECODED_YEAR.to_string());
    fields.push(Field::new(DECODED_YEAR, DataType::Int32, true));
    for m in metas {
        let dt = match m.dtype {
            IdsDType::Str => DataType::Utf8,
            IdsDType::Int => DataType::Int64,
            IdsDType::Float => DataType::Float64,
        };
        if taken.contains(&m.name) {
            return Err(ParquetError::Config(format!(
                "projected element {:?} collides with an output column name; drop it from ids",
                m.name
            )));
        }
        taken.push(m.name.clone());
        fields.push(Field::new(m.name.clone(), dt, true));
    }
    Ok(Arc::new(Schema::new(fields)))
}

/// Decode + project one chunk into the file's output shape. Output row order
/// equals input row order; null VINs decode as empty strings but stay null on
/// the passthrough column.
fn transform(
    st: &FileState,
    metas: &[IdMeta],
    batch: RecordBatch,
) -> Result<RecordBatch, ParquetError> {
    let vin_arr = cast(batch.column(st.vin_idx), &DataType::Utf8)
        .map_err(|e| ParquetError::Io(format!("casting VIN column: {e}")))?;
    let vin = vin_arr.as_string::<i32>();
    let vins: Vec<String> = (0..vin.len())
        .map(|i| {
            if vin.is_null(i) {
                String::new()
            } else {
                vin.value(i).to_string()
            }
        })
        .collect();

    // The caller-year column is cast once and reused: the kernel reads it and the
    // passthrough emits it, both as Int32.
    let year_arr = match st.year_idx {
        None => None,
        Some(yi) => Some(
            cast(batch.column(yi), &DataType::Int32)
                .map_err(|e| ParquetError::Io(format!("casting caller-year column to int: {e}")))?,
        ),
    };
    let years: Option<Vec<Option<i32>>> = year_arr.as_ref().map(|a| {
        let y = a.as_primitive::<arrow_array::types::Int32Type>();
        (0..y.len())
            .map(|i| (!y.is_null(i)).then(|| y.value(i)))
            .collect()
    });

    let out = decode_batch_ids(&vins, years.as_deref(), metas);

    let mut cols: Vec<ArrayRef> = Vec::with_capacity(metas.len() + 3);
    cols.push(vin_arr);
    cols.extend(year_arr);
    cols.push(Arc::new(Int32Array::from(out.model_year)));
    for col in out.columns {
        cols.push(match col {
            ColumnValues::Str(v) => Arc::new(StringArray::from(v)) as ArrayRef,
            ColumnValues::Int(v) => Arc::new(Int64Array::from(v)),
            ColumnValues::Float(v) => Arc::new(Float64Array::from(v)),
        });
    }
    RecordBatch::try_new(st.out_schema.clone(), cols)
        .map_err(|e| ParquetError::Io(format!("assembling output batch: {e}")))
}

/// Streaming decoder over one file or a directory of them. Eagerly opens the
/// first file so the output schema is known before the first chunk flows —
/// callers writing parquet need it even for zero-row inputs.
pub struct ParquetChunkIter {
    files: std::vec::IntoIter<PathBuf>,
    state: Option<FileState>,
    opts: ParquetOpts,
    metas: Vec<IdMeta>,
    /// Set once a chunk or a file open has failed. The reader is then parked
    /// mid-source, so resuming would silently skip whatever it could not read —
    /// stop instead.
    failed: bool,
    /// Output schema (present once the first file is open).
    pub out_schema: Option<SchemaRef>,
}

/// Open a dataset source for chunked decoding.
pub fn open_chunks(src: &Path, opts: ParquetOpts) -> Result<ParquetChunkIter, ParquetError> {
    let metas = resolve_ids(Db::embedded(), &opts.ids).map_err(ParquetError::Config)?;
    let mut iter = ParquetChunkIter {
        files: expand_src(src)?.into_iter(),
        state: None,
        opts,
        metas,
        failed: false,
        out_schema: None,
    };
    iter.advance_file()?;
    Ok(iter)
}

impl ParquetChunkIter {
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
        let st = FileState::open(&path, &self.opts, &self.metas)?;
        match &self.out_schema {
            Some(first) if first != &st.out_schema => {
                return Err(ParquetError::Config(format!(
                    "{}: passes through [{}], but the first file passes through [{}] — \
                     every file in a directory must resolve the same columns; \
                     name the VIN/year columns explicitly",
                    path.display(),
                    passthrough_names(&st.out_schema, self.metas.len()),
                    passthrough_names(first, self.metas.len()),
                )))
            }
            Some(_) => {}
            None => self.out_schema = Some(st.out_schema.clone()),
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
        loop {
            if self.state.is_none() && !self.advance_file()? {
                return Ok(None);
            }
            let st = self.state.as_mut().expect("a file is open");
            let batch = match st.reader.next().transpose() {
                Err(e) => return Err(ParquetError::Io(format!("reading row group: {e}"))),
                Ok(Some(b)) => b,
                // File drained — fall through to the next one, if any.
                Ok(None) => {
                    self.state = None;
                    continue;
                }
            };
            let st = self.state.as_ref().expect("a file is open");
            return transform(st, &self.metas, batch).map(Some);
        }
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

/// The writer truncates `dst` before the reader has finished with the source, so
/// a destination that *is* the source (or lives inside a source directory) would
/// destroy the input mid-decode. Both are caller mistakes worth refusing up
/// front, while the input is still intact.
fn check_dst_outside_src(src: &Path, dst: &Path) -> Result<(), ParquetError> {
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
    let mut iter = open_chunks(src, opts)?;
    let schema = iter
        .out_schema
        .clone()
        .ok_or_else(|| ParquetError::Config("source expanded to no files".to_string()))?;
    let out = File::create(dst).map_err(|e| ParquetError::Io(format!("{}: {e}", dst.display())))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        // One row group per chunk keeps peak write-side memory at O(chunk) too.
        .set_max_row_group_row_count(Some(row_group))
        .build();
    let mut writer = ArrowWriter::try_new(out, schema, Some(props))
        .map_err(|e| ParquetError::Io(format!("{}: {e}", dst.display())))?;
    let mut rows = 0usize;
    while let Some(batch) = iter.next_chunk()? {
        rows += batch.num_rows();
        writer
            .write(&batch)
            .map_err(|e| ParquetError::Io(format!("writing row group: {e}")))?;
    }
    writer
        .close()
        .map_err(|e| ParquetError::Io(format!("finalizing {}: {e}", dst.display())))?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

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
            ids: ids.to_vec(),
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
        let metas = resolve_ids(Db::embedded(), &opts.ids).expect("ids");
        let st = FileState::open(path, opts, &metas).expect("open");
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
        assert!(err.contains("collides with an output column"), "{err}");

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
}
