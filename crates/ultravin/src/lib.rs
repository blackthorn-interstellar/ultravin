//! ultravin — pure-Rust NHTSA vPIC VIN decoder engine.
//!
//! A full decode against the embedded rkyv artifact, targeting byte-for-byte
//! parity with the official Postgres `vpic.spvindecode`: WMI lookup,
//! schema/pattern matching, the layered sources (engine/formula patterns, make,
//! conversions, vehicle specs, defaults), per-element dedup and resolution, the
//! four-pass best-of model-year selection, the error codes, and the suggested-VIN
//! correction machinery. The same artifact also drives VIN generation
//! ([`generate`](fn@generate), [`sweep`], [`pairwise`], [`seeded`], and the
//! built-in cover) for
//! exercising a decoder with nothing else installed.

pub mod adaptive;
#[cfg(feature = "arrow")]
pub mod arrow_io;
mod batch_results;
mod checkdigit;
mod conversion;
pub mod cover;
pub mod db;
mod decode;
mod errors;
pub mod generate;
mod hash;
mod ids;
mod json;
mod keyspec;
mod matcher;
mod native_auto;
mod native_stream;
#[cfg(feature = "parquet")]
pub mod parquet_io;
pub mod predictor;
mod resolve;
#[cfg(feature = "stage-trace")]
#[doc(hidden)]
pub mod stage_trace;
pub mod tables;
mod wmi;
mod year;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "arrow")]
pub use arrow_io::{
    ArrowDecoder, ArrowError, ArrowOpts, ColumnNames, ELEMENT_ID_KEY, VARIABLE_KEY,
};
pub use batch_results::BatchResults;
pub use checkdigit::check_digit;
pub use db::Db;
#[doc(hidden)] // build-tooling hook (`vpic-import --stale-cache-report`), not API
pub use errors::recompute_valid_chars;
pub use generate::{generate, pairwise, seeded, sweep, Dimension, Filter};
pub use ids::{
    all_public_ids, decode_batch_ids, decode_batch_ids_at, resolve_columns, resolve_ids,
    ColumnSpec, ColumnValues, IdMeta, IdsBatch, IdsDType,
};
pub use matcher::sqlwild_to_regex;
pub use native_auto::{
    decode_native_stream, decode_native_stream_auto_at, NativeAutoError, NativeAutoOptions,
};
pub use native_stream::{
    decode_native_stream_at, NativeBatch, NativeStreamConfig, NativeStreamError,
};
pub use wmi::{vin_descriptor, vin_wmi};

/// Identity of the decoder and the embedded NHTSA data artifact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Provenance<'a> {
    pub data_month: &'static str,
    pub artifact_blake3: &'a str,
    pub decoder_version: &'static str,
}

/// Return the release identity carried by this build.
pub fn provenance() -> Provenance<'static> {
    Db::embedded().provenance()
}

impl Db {
    /// Identity of this decoder and loaded artifact. Artifacts from another
    /// release predate embedded month metadata, so their month is `unknown`.
    pub fn provenance(&self) -> Provenance<'_> {
        let artifact_blake3 = self.artifact_blake3();
        Provenance {
            // An override may supply a valid artifact from another month. The old
            // format does not encode its month, so do not attach this release's
            // month unless the computed content digest proves it is the pinned blob.
            data_month: if artifact_blake3 == env!("ULTRAVIN_ARTIFACT_BLAKE3") {
                env!("ULTRAVIN_DATA_MONTH")
            } else {
                "unknown"
            },
            artifact_blake3,
            decoder_version: env!("CARGO_PKG_VERSION"),
        }
    }
}

/// One resolved output element (the 15-column `spvindecode` row).
///
/// The five element-metadata columns (`group_name`/`variable`/`code`/`data_type`/
/// `decode`) borrow immutable storage owned by `Db`. Item-derived text remains
/// borrowed when it already points into the database arena and owned when
/// resolution or cleanup computed new text. This avoids per-element copies while
/// tying every borrowed field to the backing [`Db`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DecodedElement<'a> {
    pub group_name: &'a str,
    pub variable: &'a str,
    pub value: std::borrow::Cow<'a, str>,
    pub element_id: i32,
    pub attribute_id: std::borrow::Cow<'a, str>,
    pub code: &'a str,
    pub data_type: &'a str,
    pub decode: &'a str,
    pub source: std::borrow::Cow<'a, str>,
    pub pattern_id: Option<i32>,
    pub vin_schema_id: Option<i32>,
    pub keys: std::borrow::Cow<'a, str>,
    pub created_on: Option<i64>,
    pub wmi_id: Option<i32>,
    pub to_be_qced: bool,
}

/// A decoded VIN result. `'a` is the lifetime of the backing [`Db`] whose arena
/// the element-metadata columns borrow; [`decode`]/[`decode_batch`] use the
/// process-static embedded db, so they yield `DecodeResult<'static>`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DecodeResult<'a> {
    pub vin: String,
    pub wmi: String,
    pub descriptor: String,
    pub model_year: Option<i32>,
    pub error_codes: Vec<i32>,
    pub check_digit_valid: bool,
    pub corrected_vin: String,
    pub elements: Vec<DecodedElement<'a>>,
}

/// One attribute of a [`FlatResult`]: a single value, or the full list for the
/// elements that are allowed to repeat (see [`FlatResult::attributes`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum FlatValue {
    One(String),
    Many(Vec<String>),
}

/// A decoded VIN with its elements collapsed to `variable -> value`.
///
/// Same header fields as [`DecodeResult`]; `elements` is replaced by
/// `attributes`, which drops the 13 per-element provenance columns and keeps the
/// pair almost every caller actually reads. Its decode path resolves only the
/// output values, avoiding owned provenance strings; Python marshalling then
/// builds one attribute entry instead of a 15-key dict per element.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FlatResult<'a> {
    pub vin: String,
    pub wmi: String,
    pub descriptor: String,
    pub model_year: Option<i32>,
    pub error_codes: Vec<i32>,
    pub check_digit_valid: bool,
    pub corrected_vin: String,
    /// `variable -> value`, in the element order of [`DecodeResult::elements`].
    /// Kept as ordered pairs rather than a map so the order survives into
    /// Python/JSON; the keys are unique, so it serializes as a JSON object.
    #[serde(serialize_with = "serialize_pairs")]
    pub attributes: Vec<(&'a str, FlatValue)>,
}

fn serialize_pairs<S: serde::Serializer>(
    pairs: &[(&str, FlatValue)],
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(pairs.iter().map(|(k, v)| (k, v)))
}

impl<'a> From<DecodeResult<'a>> for FlatResult<'a> {
    /// Collapse `elements` to `attributes`.
    ///
    /// The dedup-exempt elements ([`tables::EXEMPT_ELEMENTS`] — `Note`,
    /// `Other Engine Info`, …) are the only ones the decoder may emit more than
    /// once per VIN, and each row is an independent note rather than a competing
    /// value. They are therefore **always** [`FlatValue::Many`], even at length
    /// one, so a consumer's field type never depends on the data in front of it.
    /// Every other variable takes its first occurrence; a second one is
    /// unreachable with the current archive (no model maps to more than one make)
    /// and would be a data change worth catching in the refresh gates.
    fn from(r: DecodeResult<'a>) -> Self {
        let attributes = flatten_values(
            r.elements
                .into_iter()
                .map(|e| (e.variable, e.element_id, e.value.into_owned())),
        );
        FlatResult {
            vin: r.vin,
            wmi: r.wmi,
            descriptor: r.descriptor,
            model_year: r.model_year,
            error_codes: r.error_codes,
            check_digit_valid: r.check_digit_valid,
            corrected_vin: r.corrected_vin,
            attributes,
        }
    }
}

/// Keep the first value for each variable name, collecting repeatable notes.
fn flatten_values<'a>(
    values: impl Iterator<Item = (&'a str, i32, String)>,
) -> Vec<(&'a str, FlatValue)> {
    let mut attributes: Vec<(&'a str, FlatValue)> = Vec::with_capacity(values.size_hint().0);
    let mut seen: std::collections::HashMap<&'a str, usize, hash::FxBuildHasher> =
        std::collections::HashMap::default();
    for (variable, element_id, value) in values {
        match seen.get(variable) {
            Some(&i) => {
                if let (_, FlatValue::Many(list)) = &mut attributes[i] {
                    list.push(value);
                }
            }
            None => {
                seen.insert(variable, attributes.len());
                let value = if tables::is_exempt(element_id) {
                    FlatValue::Many(vec![value])
                } else {
                    FlatValue::One(value)
                };
                attributes.push((variable, value));
            }
        }
    }
    attributes
}

/// With unique variable names and element-sorted rows, repeats are adjacent.
/// External databases with duplicate names retain the general name-based path.
fn flatten_unique_variables<'a>(
    values: impl Iterator<Item = (&'a str, i32, String)>,
) -> Vec<(&'a str, FlatValue)> {
    let mut attributes: Vec<(&str, FlatValue)> = Vec::with_capacity(values.size_hint().0);
    let mut previous = None;
    for (name, id, value) in values {
        if previous == Some(id) {
            if let Some((_, FlatValue::Many(list))) = attributes.last_mut() {
                list.push(value);
            }
        } else {
            previous = Some(id);
            attributes.push((
                name,
                if tables::is_exempt(id) {
                    FlatValue::Many(vec![value])
                } else {
                    FlatValue::One(value)
                },
            ));
        }
    }
    attributes
}

/// The element's `Decode` text when it is one `project` emits, `None` when the
/// element never reaches output (no Decode text, or private). The single gate for
/// "can this element appear in a result", shared by the projection, the
/// multi-valued list and the exported element table so they cannot drift.
pub fn public_decode<'a>(db: &'a Db, e: &'a tables::ArchivedElement) -> Option<&'a str> {
    let decode = db.s(e.decode.to_native());
    (e.decode_present && !decode.is_empty() && !e.isprivate).then_some(decode)
}

/// The variable names whose [`FlatResult`] value is always a list. Only the
/// exempt elements that can actually reach output — an exempt element the
/// projection filters out is never a key, so advertising it would be a lie.
pub fn multi_valued_variables(db: &Db) -> Vec<&str> {
    tables::EXEMPT_ELEMENTS
        .iter()
        .filter_map(|id| db.element_by_id(*id))
        .filter(|e| public_decode(db, e).is_some())
        .map(|e| db.s(e.name.to_native()))
        .collect()
}

fn opt_i32(v: i32) -> Option<i32> {
    if v == tables::NULL_I32 {
        None
    } else {
        Some(v)
    }
}

fn opt_i64(v: i64) -> Option<i64> {
    if v == tables::NULL_I64 {
        None
    } else {
        Some(v)
    }
}

/// `replace(value, [\t\r\n], ' ')` — but only allocate when a control char is
/// actually present. The clean case (the overwhelming majority) moves the value's
/// existing owned `String` straight through; a borrowed literal still pays one
/// (rare, short) copy via `into_owned`.
fn scrub(v: std::borrow::Cow<'_, str>) -> String {
    scrub_value(v).into_owned()
}

/// Column builders can copy clean borrowed text directly into their final
/// buffers; only values containing control characters need an intermediate.
fn scrub_value(v: std::borrow::Cow<'_, str>) -> std::borrow::Cow<'_, str> {
    if has_line_control(v.as_bytes()) {
        std::borrow::Cow::Owned(v.replace(['\t', '\r', '\n'], " "))
    } else {
        v
    }
}

/// Whether `bytes` holds a tab, CR or LF. Values are short and almost never
/// contain one, so test eight bytes at a time for any byte below 0x0E and
/// look closer only then.
fn has_line_control(bytes: &[u8]) -> bool {
    const ONES: u64 = u64::from_ne_bytes([1; 8]);
    let below_0e = |word: [u8; 8]| {
        let w = u64::from_ne_bytes(word);
        w.wrapping_sub(ONES * 0x0E) & !w & (ONES * 0x80)
    };
    let suspect = match bytes.last_chunk::<8>() {
        // The final (possibly overlapping) word covers the tail.
        Some(&last) => {
            let (words, _) = bytes.as_chunks::<8>();
            words
                .iter()
                .fold(below_0e(last), |acc, &w| acc | below_0e(w))
        }
        None => bytes.iter().fold(0, |acc, &b| acc | u64::from(b < 0x0E)),
    };
    suspect != 0 && bytes.iter().any(|b| matches!(b, b'\t' | b'\r' | b'\n'))
}

/// Convert Unix epoch seconds to the calendar year (Hinnant's civil algorithm).
fn epoch_to_year(secs: i64) -> i32 {
    let days = secs.div_euclid(86400);
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }) as i32
}

/// Unix epoch seconds by the system clock, saturating to 0 before 1970.
fn now_secs() -> i64 {
    #[cfg(test)]
    if let Some(secs) = clock_tests::read_clock() {
        return secs;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The current model year by the system clock, as the decoder reckons it.
pub fn current_year() -> i32 {
    let secs = now_secs();
    epoch_to_year(secs)
}

/// "Now" in the units [`decode_with`], [`decode_full`] and [`generate`](fn@generate) take.
///
/// Those take the clock as an argument so their output is a pure function of
/// their inputs; this is the one place that reads it, for callers who do want
/// the system clock.
pub fn now_micros() -> i64 {
    now_secs() * 1_000_000
}

/// The model year a given clock reading falls in, in the units [`now_micros`]
/// returns. [`current_year`] is this applied to the system clock.
///
/// A caller who supplies its own clock should derive the year from that same
/// reading so the instant and its calendar-year window cannot straddle midnight
/// on New Year's Eve.
pub fn current_year_at(now_micros: i64) -> i32 {
    epoch_to_year(now_micros.div_euclid(1_000_000))
}

/// Decode a VIN using the embedded database and the system clock.
///
/// `year` is the optional caller-supplied model year (the proc's `@year`): when
/// it lands in `[1980, current_year + 2]` and differs from the VIN-derived
/// candidates it gets its own decode pass, which competes in the best-pass
/// scoring (with the +10000 bonus for a pass whose decoded year matches it).
/// In or out of that window, a caller year that contradicts a pass's decoded
/// year flags error 12 on that pass.
pub fn decode(input: &str, year: Option<i32>) -> DecodeResult<'static> {
    Db::embedded().decode(input, year)
}

/// [`decode`] at an explicit instant, for reproducible decoding.
pub fn decode_at(input: &str, year: Option<i32>, now_micros: i64) -> DecodeResult<'static> {
    Db::embedded().decode_at(input, year, now_micros)
}

/// [`decode`] with the [`FlatResult`] shape: elements collapsed to
/// `variable -> value`, the 13 per-element provenance columns dropped.
pub fn decode_flat(input: &str, year: Option<i32>) -> FlatResult<'static> {
    decode_flat_at(input, year, now_micros())
}

/// [`decode_flat`] at an explicit instant.
pub fn decode_flat_at(input: &str, year: Option<i32>, now_micros: i64) -> FlatResult<'static> {
    decode_items(
        Db::embedded(),
        input,
        now_micros,
        current_year_at(now_micros),
        year,
    )
    .flat()
}

/// Decode a VIN against an explicit database and clock (injectable for tests),
/// with no caller-supplied model year.
pub fn decode_with<'a>(
    db: &'a Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
) -> DecodeResult<'a> {
    decode_full(db, input, now_micros, current_year, None)
}

/// Decode many VINs in parallel over the shared (immutable) embedded archive.
///
/// The clock is read once so a batch is internally consistent; each VIN is then
/// decoded independently across rayon's thread pool. Output order matches
/// `inputs`. `years` optionally supplies a caller model year per VIN (the batch
/// equivalent of [`decode`]'s `year` — vPIC's batch API carries one per line);
/// a missing entry or a `None` decodes that VIN with no year. Per-VIN output is
/// identical to calling [`decode`] with the matching year.
pub fn decode_batch(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
) -> Vec<DecodeResult<'static>> {
    Db::embedded().decode_batch(inputs, years)
}

/// [`decode_batch`] at an explicit instant.
pub fn decode_batch_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
) -> Vec<DecodeResult<'static>> {
    Db::embedded().decode_batch_at(inputs, years, now_micros)
}

/// [`decode_batch`] with parallel cleanup when the returned container is dropped.
pub fn decode_batch_managed(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
) -> BatchResults<DecodeResult<'static>> {
    Db::embedded().decode_batch_managed(inputs, years)
}

/// [`decode_batch_managed`] at an explicit instant.
pub fn decode_batch_managed_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
) -> BatchResults<DecodeResult<'static>> {
    Db::embedded().decode_batch_managed_at(inputs, years, now_micros)
}

/// [`decode_batch`] with the [`FlatResult`] shape. Flattening runs inside the
/// parallel region, so only the (much smaller) marshalling is left to the caller.
pub fn decode_batch_flat(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
) -> Vec<FlatResult<'static>> {
    Db::embedded().decode_batch_flat(inputs, years)
}

/// [`decode_batch_flat`] at an explicit instant.
pub fn decode_batch_flat_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
) -> Vec<FlatResult<'static>> {
    Db::embedded().decode_batch_flat_at(inputs, years, now_micros)
}

/// [`decode_batch_flat`] with parallel cleanup when the returned container is dropped.
pub fn decode_batch_flat_managed(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
) -> BatchResults<FlatResult<'static>> {
    Db::embedded().decode_batch_flat_managed(inputs, years)
}

/// [`decode_batch_flat_managed`] at an explicit instant.
pub fn decode_batch_flat_managed_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
) -> BatchResults<FlatResult<'static>> {
    Db::embedded().decode_batch_flat_managed_at(inputs, years, now_micros)
}

/// The decode API against an explicit database, for a [`Db`] you loaded yourself
/// ([`Db::open`] / [`Db::from_bytes`]). The free functions above are these three
/// on [`Db::embedded`]; results borrow the `Db`'s arena for the element metadata.
impl Db {
    /// [`decode`](fn@decode) against this database, with the system clock.
    pub fn decode(&self, input: &str, year: Option<i32>) -> DecodeResult<'_> {
        let secs = now_secs();
        decode_full(self, input, secs * 1_000_000, epoch_to_year(secs), year)
    }

    /// [`Db::decode`] at an explicit instant.
    pub fn decode_at(&self, input: &str, year: Option<i32>, now_micros: i64) -> DecodeResult<'_> {
        decode_full(self, input, now_micros, current_year_at(now_micros), year)
    }

    /// [`decode_batch`](fn@decode_batch) against this database.
    pub fn decode_batch(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
    ) -> Vec<DecodeResult<'_>> {
        batch_at(self, inputs, years, now_micros(), RawResult::full)
    }

    /// [`Db::decode_batch`] at an explicit instant.
    pub fn decode_batch_at(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        now_micros: i64,
    ) -> Vec<DecodeResult<'_>> {
        batch_at(self, inputs, years, now_micros, RawResult::full)
    }

    /// [`Db::decode_batch`] with parallel cleanup for large returned batches.
    pub fn decode_batch_managed(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
    ) -> BatchResults<DecodeResult<'_>> {
        BatchResults::new(self.decode_batch(inputs, years))
    }

    /// [`Db::decode_batch_managed`] at an explicit instant.
    pub fn decode_batch_managed_at(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        now_micros: i64,
    ) -> BatchResults<DecodeResult<'_>> {
        BatchResults::new(self.decode_batch_at(inputs, years, now_micros))
    }

    /// [`decode_batch_flat`](fn@decode_batch_flat) against this database.
    pub fn decode_batch_flat(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
    ) -> Vec<FlatResult<'_>> {
        batch_at(self, inputs, years, now_micros(), RawResult::flat)
    }

    /// [`Db::decode_batch_flat`] at an explicit instant.
    pub fn decode_batch_flat_at(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        now_micros: i64,
    ) -> Vec<FlatResult<'_>> {
        batch_at(self, inputs, years, now_micros, RawResult::flat)
    }

    /// [`Db::decode_batch_flat`] with parallel cleanup for large returned batches.
    pub fn decode_batch_flat_managed(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
    ) -> BatchResults<FlatResult<'_>> {
        BatchResults::new(self.decode_batch_flat(inputs, years))
    }

    /// [`Db::decode_batch_flat_managed`] at an explicit instant.
    pub fn decode_batch_flat_managed_at(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        now_micros: i64,
    ) -> BatchResults<FlatResult<'_>> {
        BatchResults::new(self.decode_batch_flat_at(inputs, years, now_micros))
    }
}

/// The thread pool the batch paths run on, rebuilt whenever the pid changes.
///
/// rayon's *global* pool spawns its workers once per process, and `fork()` copies
/// only the calling thread: the child inherits the pool's bookkeeping but none of
/// its threads, so its first `par_iter` queues a job no worker will ever steal and
/// blocks forever. That is the ordinary shape of a fork-based Python deployment —
/// gunicorn prefork after a warmup decode, `multiprocessing` with the `fork` start
/// method. Keying the cached pool on `process::id()` makes the child notice it is
/// not the process that built the pool and build its own.
fn batch_pool() -> Arc<rayon::ThreadPool> {
    /// The cached pool, tagged with the pid that built it.
    type Owned = Mutex<Option<(u32, Arc<rayon::ThreadPool>)>>;

    static POOL: OnceLock<Owned> = OnceLock::new();

    let pid = std::process::id();
    // Poisoning only means another caller panicked while building; whatever is
    // cached is still sound to read.
    let mut slot = POOL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some((_, pool)) = slot.as_ref().filter(|(owner, _)| *owner == pid) {
        return Arc::clone(pool);
    }
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .build()
            .expect("rayon thread pool"),
    );
    *slot = Some((pid, Arc::clone(&pool)));
    pool
}

thread_local! {
    static PRIVATE_CALIBRATION_POOL_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Keep nested batch work on the private Rayon pool installed by predictor
/// calibration. The scope is thread-local so ordinary callers still use the
/// pid-owned pool, including after a fork. The guard restores routing on panic.
pub(crate) fn with_private_calibration_pool_scope<T>(run: impl FnOnce() -> T) -> T {
    struct Reset<'a> {
        depth: &'a std::cell::Cell<usize>,
        previous: usize,
    }

    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            self.depth.set(self.previous);
        }
    }

    PRIVATE_CALIBRATION_POOL_DEPTH.with(|depth| {
        let previous = depth.get();
        depth.set(previous.saturating_add(1));
        let _reset = Reset { depth, previous };
        run()
    })
}

pub(crate) fn install_batch_work<T: Send>(run: impl FnOnce() -> T + Send) -> T {
    if PRIVATE_CALIBRATION_POOL_DEPTH.with(|depth| depth.get() > 0) {
        run()
    } else {
        batch_pool().install(run)
    }
}

/// The caller year for input `i`: `years` may be absent entirely, shorter than
/// the inputs, or hold `None` gaps — all mean "no year for this VIN".
fn year_at(years: Option<&[Option<i32>]>, i: usize) -> Option<i32> {
    years.and_then(|ys| ys.get(i)).copied().flatten()
}

/// The decode order for a batch: input indices sorted by VIN, so neighbouring
/// work shares a WMI (and therefore its schemas, patterns and per-thread memos).
///
/// A decode's cost is dominated by walking that WMI's pattern keys, which are
/// scattered through the 80 MB archive — decoding a WMI-sorted corpus measured
/// ~11% faster than the same VINs shuffled, on identical work. Sorting 3-byte
/// prefixes costs microseconds against that. Output order is restored before
/// return; only the visiting order changes, and a decode depends on nothing but
/// its own VIN, so results are unaffected.
fn locality_order(inputs: &[String]) -> Vec<u32> {
    use rayon::prelude::*;

    let mut order: Vec<u32> = (0..inputs.len() as u32).collect();
    // The WMI is the first three characters; the next five pick the schema's
    // pattern keys, so sorting on the descriptor prefix clusters both.
    order.par_sort_unstable_by_key(|&i| locality_key(&inputs[i as usize]));
    order
}

fn locality_key(input: &str) -> u64 {
    let bytes = input.as_bytes();
    let mut key = [0u8; 8];
    let len = bytes.len().min(8);
    key[..len].copy_from_slice(&bytes[..len]);
    u64::from_be_bytes(key)
}

/// Run work in locality order while writing each value directly to its final
/// input-order slot. The sorted work list contains disjoint safe references and
/// never owns a result. On panic, initialized `Option`s drop normally and empty
/// slots do nothing; successful extraction performs only shallow moves.
fn collect_in_input_order<T: Send>(
    inputs: &[String],
    decode: impl Fn(usize) -> T + Sync,
) -> Vec<T> {
    use rayon::prelude::*;

    #[cfg(feature = "stage-trace")]
    let batch_id = stage_trace::sample_decode_batch();
    #[cfg(feature = "stage-trace")]
    let mut total_span = stage_trace::WorkerSpan::new("decode_total", batch_id);
    #[cfg(feature = "stage-trace")]
    total_span.rows(inputs.len());

    #[cfg(feature = "stage-trace")]
    let mut slots: Vec<Option<T>> =
        stage_trace::serial("decode_slot_setup", batch_id, inputs.len(), || {
            (0..inputs.len()).map(|_| None).collect()
        });
    #[cfg(not(feature = "stage-trace"))]
    let mut slots: Vec<Option<T>> = (0..inputs.len()).map(|_| None).collect();
    #[cfg(feature = "stage-trace")]
    let mut work: Vec<_> =
        stage_trace::serial("decode_worklist_setup", batch_id, inputs.len(), || {
            slots.iter_mut().enumerate().collect()
        });
    #[cfg(not(feature = "stage-trace"))]
    let mut work: Vec<_> = slots.iter_mut().enumerate().collect();
    #[cfg(feature = "stage-trace")]
    stage_trace::serial("decode_locality_sort", batch_id, inputs.len(), || {
        work.par_sort_unstable_by_key(|(index, _)| locality_key(&inputs[*index]));
    });
    #[cfg(not(feature = "stage-trace"))]
    work.par_sort_unstable_by_key(|(index, _)| locality_key(&inputs[*index]));
    #[cfg(feature = "stage-trace")]
    if batch_id.is_some() {
        let mut parallel_span = stage_trace::WorkerSpan::new("decode_parallel", batch_id);
        parallel_span.rows(inputs.len());
        work.into_par_iter().for_each_init(
            || stage_trace::WorkerSpan::new("decode_worker", batch_id),
            |span, (index, slot)| {
                span.row();
                *slot = Some(decode(index));
            },
        );
    } else {
        work.into_par_iter()
            .for_each(|(index, slot)| *slot = Some(decode(index)));
    }
    #[cfg(not(feature = "stage-trace"))]
    work.into_par_iter()
        .for_each(|(index, slot)| *slot = Some(decode(index)));
    let extract = || {
        slots
            .into_iter()
            .map(|slot| slot.expect("every batch output slot was initialized"))
            .collect()
    };
    #[cfg(feature = "stage-trace")]
    {
        stage_trace::serial("decode_extract", batch_id, inputs.len(), extract)
    }
    #[cfg(not(feature = "stage-trace"))]
    {
        extract()
    }
}

/// Shared body of the batch paths: decode every input in parallel over the shared
/// archive, mapped through `shape`. Output order matches `inputs`.
fn batch_at<'a, T: Send>(
    db: &'a Db,
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
    shape: impl Fn(RawResult<'a>) -> T + Sync,
) -> Vec<T> {
    let current_year = current_year_at(now_micros);
    install_batch_work(|| {
        collect_in_input_order(inputs, |i| {
            shape(decode_items(
                db,
                &inputs[i],
                now_micros,
                current_year,
                year_at(years, i),
            ))
        })
    })
}

fn restore_input_order<T>(order: &mut [u32], values: &mut [T]) {
    debug_assert_eq!(order.len(), values.len());
    for position in 0..order.len() {
        while order[position] as usize != position {
            let destination = order[position] as usize;
            order.swap(position, destination);
            values.swap(position, destination);
        }
    }
}

/// Decode one VIN to a compact JSON object string (same shape as the [`decode`]
/// dict). Serializing in Rust avoids the per-field Python dict construction.
pub fn decode_json(input: &str, year: Option<i32>) -> String {
    decode_json_at(input, year, now_micros())
}

/// [`decode_json`] at an explicit instant.
pub fn decode_json_at(input: &str, year: Option<i32>, now_micros: i64) -> String {
    json::encode(decode_items(
        Db::embedded(),
        input,
        now_micros,
        current_year_at(now_micros),
        year,
    ))
}

/// [`decode_json`] with the [`FlatResult`] shape.
pub fn decode_json_flat(input: &str, year: Option<i32>) -> String {
    serde_json::to_string(&decode_flat(input, year)).expect("FlatResult is infallibly serializable")
}

/// [`decode_json_flat`] at an explicit instant.
pub fn decode_json_flat_at(input: &str, year: Option<i32>, now_micros: i64) -> String {
    serde_json::to_string(&decode_flat_at(input, year, now_micros))
        .expect("FlatResult is infallibly serializable")
}

/// Decode many VINs to a single compact JSON array string, in parallel.
///
/// Decode **and** serialization run with the GIL released across rayon's pool;
/// only the final array assembly is serial. This is the high-throughput batch
/// path: the caller receives one string (one Python allocation) instead of a
/// list of ~60-key dicts per VIN, sidestepping the GIL-serial marshalling that
/// otherwise caps `decode_batch`. `json.loads` of the output equals
/// `decode_batch` element-for-element.
pub fn decode_batch_json(inputs: &[String], years: Option<&[Option<i32>]>) -> String {
    decode_batch_json_at(inputs, years, now_micros())
}

/// [`decode_batch_json`] at an explicit instant.
pub fn decode_batch_json_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
) -> String {
    batch_json_framed(inputs, years, now_micros, json::encode, false)
}

/// [`decode_batch_json`] with the [`FlatResult`] shape.
pub fn decode_batch_json_flat(inputs: &[String], years: Option<&[Option<i32>]>) -> String {
    decode_batch_json_flat_at(inputs, years, now_micros())
}

/// [`decode_batch_json_flat`] at an explicit instant.
pub fn decode_batch_json_flat_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
) -> String {
    batch_json_framed(
        inputs,
        years,
        now_micros,
        |r| serde_json::to_string(&r.flat()).expect("FlatResult is infallibly serializable"),
        false,
    )
}

/// Decode a batch as newline-delimited JSON, including a trailing newline.
pub fn decode_batch_jsonl_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
    full: bool,
) -> String {
    if full {
        batch_json_framed(inputs, years, now_micros, json::encode, true)
    } else {
        batch_json_framed(
            inputs,
            years,
            now_micros,
            |r| serde_json::to_string(&r.flat()).expect("FlatResult is infallibly serializable"),
            true,
        )
    }
}

fn batch_json_framed(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
    encode: impl Fn(RawResult<'static>) -> String + Sync,
    jsonl: bool,
) -> String {
    use rayon::prelude::*;
    let current_year = current_year_at(now_micros);
    let db = Db::embedded();
    // Fuse decode + serialize so both happen in parallel; reorder the object
    // strings in place to avoid two additional whole-batch vectors.
    let parts: Vec<String> = install_batch_work(|| {
        let mut order = locality_order(inputs);
        let mut decoded = Vec::new();
        order
            .par_iter()
            .map(|&i| {
                encode(decode_items(
                    db,
                    &inputs[i as usize],
                    now_micros,
                    current_year,
                    year_at(years, i as usize),
                ))
            })
            .collect_into_vec(&mut decoded);
        restore_input_order(&mut order, &mut decoded);
        decoded
    });
    let cap = parts.iter().map(|part| part.len() + 1).sum::<usize>() + usize::from(!jsonl) * 2;
    let mut out = String::with_capacity(cap);
    if !jsonl {
        out.push('[');
    }
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            out.push(if jsonl { '\n' } else { ',' });
        }
        out.push_str(&part);
    }
    if jsonl {
        if !inputs.is_empty() {
            out.push('\n');
        }
    } else {
        out.push(']');
    }
    out
}

/// One decode pass (a single `spvindecode_core` invocation): its items (with the
/// 142/143/144/156/191/196 corrections appended, values still pre-resolution) and
/// the metadata the scorer and result need.
struct Pass<'a> {
    id: i32,
    model_year: Option<i32>,
    items: Vec<decode::DecodingItem<'a>>,
    /// Default rows left out of `items`: the vehicle type, and the item count
    /// they would have followed (before the correction items).
    defaults: Option<(i32, usize)>,
    codes: Vec<i32>,
    corrected_vin: String,
    check_digit_valid: bool,
}

/// Normalize raw decode input into the byte-safe VIN the engine works on.
///
/// Everything downstream — [`vin_wmi`], [`decode::build_var_keys_stack`]'s
/// `&vin[3..8]`/`&vin[9..17]` slices, the check-digit and error-code scans —
/// indexes the VIN *by byte* on the assumption that one byte is one character. That holds only
/// for ASCII: a multibyte char (`é`, `Ł`, …) puts a UTF-8 char boundary
/// mid-index and panics a byte slice. So map every non-ASCII char to one
/// representative invalid ASCII byte (`&`) here, once, at the single entry seam —
/// downstream byte indexing is then unconditionally safe, and a non-ASCII char
/// decodes exactly as an invalid ASCII `&` would at the same character position.
///
/// The all-ASCII case (every real VIN) writes the trimmed input into the supplied
/// buffer and uppercases it in place, allowing native result slots to reuse it.
/// Non-ASCII input maps *before* trimming so the result is byte-for-byte what
/// decoding the `&`-substituted string would produce: a non-ASCII whitespace
/// char becomes a non-whitespace `&` that is kept, not trimmed away.
#[cfg(test)]
fn sanitize(input: &str) -> String {
    sanitize_into(input, String::new())
}

fn sanitize_into(input: &str, mut output: String) -> String {
    output.clear();
    if input.is_ascii() {
        output.push_str(input.trim());
        output.make_ascii_uppercase();
        return output;
    }
    output.extend(input.chars().map(|c| if c.is_ascii() { c } else { '&' }));
    let trimmed = output.trim();
    let start = trimmed.as_ptr() as usize - output.as_ptr() as usize;
    let end = start + trimmed.len();
    output.drain(end..);
    output.drain(..start);
    output.make_ascii_uppercase();
    output
}

/// Build `fVinDescriptor` directly from normalized ASCII-uppercase output.
///
/// The public [`vin_descriptor`] accepts arbitrary text, so it normalizes through
/// an intermediate byte vector and then creates an uppercase `String`. At this
/// internal call site normalization has already happened. Writing the masked,
/// padded 11/14 bytes directly saves that intermediate allocation and copy.
#[cfg(test)]
fn sanitized_descriptor(vin: &str) -> String {
    sanitized_descriptor_into(vin, String::new())
}

fn sanitized_descriptor_into(vin: &str, mut descriptor: String) -> String {
    debug_assert!(vin.is_ascii());
    debug_assert!(!vin.bytes().any(|byte| byte.is_ascii_lowercase()));
    let bytes = vin.as_bytes();
    let take = if bytes.get(2) == Some(&b'9') { 14 } else { 11 };
    descriptor.clear();
    descriptor.reserve(take);
    for index in 0..take {
        descriptor.push(if index == 8 {
            '*'
        } else {
            bytes.get(index).copied().unwrap_or(b'*') as char
        });
    }
    descriptor
}

fn sanitized_wmi_into(vin: &str, mut wmi: String) -> String {
    let bytes = vin.as_bytes();
    wmi.clear();
    wmi.push_str(&vin[..bytes.len().min(3)]);
    if bytes.get(2) == Some(&b'9') && bytes.len() >= 14 {
        wmi.push_str(&vin[11..14]);
    }
    wmi
}

/// The full wrapper (`vpic.spvindecode`): up to 4 best-of passes, scoring, and
/// the GroupName-ordered projection. `caller_year` is the optional caller MY.
pub fn decode_full<'a>(
    db: &'a Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
    caller_year: Option<i32>,
) -> DecodeResult<'a> {
    decode_items(db, input, now_micros, current_year, caller_year).full()
}

pub(crate) fn decode_full_reusing<'a>(
    db: &'a Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
    caller_year: Option<i32>,
    previous: DecodeResult<'a>,
    workspace: &mut DecodeWorkspace<'a>,
) -> DecodeResult<'a> {
    let DecodeResult {
        vin,
        wmi,
        descriptor,
        error_codes,
        corrected_vin,
        mut elements,
        ..
    } = previous;
    drop((error_codes, corrected_vin));
    elements.clear();
    decode_items_with_pruning_and_buffers_workspace(
        db,
        input,
        now_micros,
        current_year,
        caller_year,
        true,
        vin,
        wmi,
        descriptor,
        workspace,
    )
    .full_into_workspace(elements, workspace)
}

pub(crate) fn decode_full_with_workspace<'a>(
    db: &'a Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
    caller_year: Option<i32>,
    workspace: &mut DecodeWorkspace<'a>,
) -> DecodeResult<'a> {
    decode_items_with_pruning_and_buffers_workspace(
        db,
        input,
        now_micros,
        current_year,
        caller_year,
        true,
        String::new(),
        String::new(),
        String::new(),
        workspace,
    )
    .full_into_workspace(Vec::new(), workspace)
}

pub(crate) struct DecodeWorkspace<'a> {
    item_vectors: [Vec<decode::DecodingItem<'a>>; 3],
    item_vectors_len: usize,
    passes: Vec<Pass<'a>>,
}

impl Default for DecodeWorkspace<'_> {
    fn default() -> Self {
        Self {
            item_vectors: std::array::from_fn(|_| Vec::new()),
            item_vectors_len: 0,
            passes: Vec::new(),
        }
    }
}

impl<'a> DecodeWorkspace<'a> {
    fn take_items(&mut self) -> Vec<decode::DecodingItem<'a>> {
        if self.item_vectors_len == 0 {
            Vec::new()
        } else {
            self.item_vectors_len -= 1;
            std::mem::take(&mut self.item_vectors[self.item_vectors_len])
        }
    }

    fn put_items(&mut self, mut items: Vec<decode::DecodingItem<'a>>) {
        const MAX_RETAINED_ITEMS: usize = 256;
        items.clear();
        if items.capacity() <= MAX_RETAINED_ITEMS && self.item_vectors_len < self.item_vectors.len()
        {
            self.item_vectors[self.item_vectors_len] = items;
            self.item_vectors_len += 1;
        }
    }

    fn take_passes(&mut self) -> Vec<Pass<'a>> {
        std::mem::take(&mut self.passes)
    }

    fn put_passes(&mut self, mut passes: Vec<Pass<'a>>) {
        const MAX_RETAINED_PASSES: usize = 4;
        passes.clear();
        if passes.capacity() <= MAX_RETAINED_PASSES {
            self.passes = passes;
        }
    }
}

/// Winning pass, before output-specific value resolution and projection.
/// Flat and column callers never need owned provenance strings.
struct RawResult<'a> {
    db: &'a Db,
    vin: String,
    wmi: String,
    descriptor: String,
    model_year: Option<i32>,
    error_codes: Vec<i32>,
    check_digit_valid: bool,
    corrected_vin: String,
    items: Vec<decode::DecodingItem<'a>>,
    /// Default rows not yet in `items` (see [`Db::defaults_independent`]).
    defaults: Option<Defaults>,
}

/// The winning pass's deferred default rows: bit `i` of `mask` selects the
/// vehicle type's template row `i`; they belong at item index `at`.
#[derive(Clone, Copy)]
struct Defaults {
    vehicle_type: i32,
    mask: u64,
    at: usize,
}

/// The embedded database's default rows as finished output records, per
/// vehicle type and in template order (`None` when not projected), each with
/// its output rank. They borrow only `'static` storage, so a full result can
/// copy them instead of building an item and projecting it per VIN.
type DefaultElements = hash::IntMap<i32, Box<[Option<(u16, DecodedElement<'static>)>]>>;

/// One vehicle type's finished default records and the mask of those that apply.
type DefaultRecords = Option<(&'static [Option<(u16, DecodedElement<'static>)>], u64)>;

fn default_elements(db: &Db) -> Option<&'static DefaultElements> {
    static ELEMENTS: std::sync::OnceLock<DefaultElements> = std::sync::OnceLock::new();
    let embedded = Db::embedded_raw();
    if !std::ptr::eq(db, embedded) || !embedded.defaults_independent() {
        return None;
    }
    Some(ELEMENTS.get_or_init(|| {
        let ranks = embedded.output_ranks();
        let projection_meta = embedded.projection_meta_lookup();
        embedded
            .default_vehicle_types()
            .map(|vehicle_type| {
                let rows = embedded
                    .default_templates_for(vehicle_type)
                    .iter()
                    .map(|dv| {
                        let meta = projection_meta(dv.element_id)?;
                        let rank = *ranks.get(usize::try_from(dv.element_id).ok()?)?;
                        let attribute_id = embedded.s(dv.attribute_id);
                        let value = if dv.not_applicable {
                            std::borrow::Cow::Borrowed("Not Applicable")
                        } else {
                            resolve::felement_attribute_value(
                                embedded,
                                dv.element_id,
                                std::borrow::Cow::Borrowed(attribute_id),
                            )
                        };
                        Some((
                            rank,
                            DecodedElement {
                                group_name: &meta.group_name,
                                variable: &meta.variable,
                                value: scrub_value(value),
                                element_id: dv.element_id,
                                attribute_id: std::borrow::Cow::Borrowed(attribute_id),
                                code: &meta.code,
                                data_type: &meta.data_type,
                                decode: &meta.decode,
                                source: std::borrow::Cow::Borrowed("Default"),
                                pattern_id: None,
                                vin_schema_id: None,
                                keys: std::borrow::Cow::Borrowed(""),
                                created_on: opt_i64(dv.created_on),
                                wmi_id: None,
                                to_be_qced: false,
                            },
                        ))
                    })
                    .collect();
                (vehicle_type, rows)
            })
            .collect()
    }))
}

/// Year-independent database and validation facts for one sanitized VIN.
struct VinPassContext<'a> {
    /// First row regardless of publication, matching `wmi_any` semantics.
    any_wmi: Option<&'a tables::ArchivedWmi>,
    /// First row public at this decode's fixed clock.
    public_wmi: Option<&'a tables::ArchivedWmi>,
    is_car_mpv_lt: bool,
    is_vin_exception: bool,
}

impl<'a> VinPassContext<'a> {
    fn new(db: &'a Db, vin: &str, var_wmi: &str, now_micros: i64) -> Self {
        let (any_wmi, public_wmi) = db.wmi_context(var_wmi, now_micros);
        Self {
            any_wmi,
            public_wmi,
            is_car_mpv_lt: any_wmi
                .map(tables::ArchivedWmi::is_car_mpv_lt)
                .unwrap_or(false),
            is_vin_exception: db.vinexception_checkdigit(vin),
        }
    }
}

impl<'a> RawResult<'a> {
    /// Put deferred default rows into `items`, exactly where the core pass
    /// would have placed them. Every item-based output shape starts here.
    fn materialize_defaults(&mut self) {
        if let Some(d) = self.defaults.take() {
            decode::insert_default_values(self.db, &mut self.items, d.vehicle_type, d.mask, d.at);
        }
    }

    /// Deferred defaults as finished records when available, else materialized.
    fn default_records(&mut self) -> DefaultRecords {
        let d = self.defaults?;
        let Some(rows) = default_elements(self.db).and_then(|by_type| by_type.get(&d.vehicle_type))
        else {
            self.materialize_defaults();
            return None;
        };
        self.defaults = None;
        Some((rows, d.mask))
    }

    fn full(mut self) -> DecodeResult<'a> {
        let defaults = self.default_records();
        resolve::resolve_xxx(self.db, &mut self.items);
        DecodeResult {
            vin: self.vin,
            wmi: self.wmi,
            descriptor: self.descriptor,
            model_year: self.model_year,
            error_codes: self.error_codes,
            check_digit_valid: self.check_digit_valid,
            corrected_vin: self.corrected_vin,
            elements: project(self.db, self.items, defaults),
        }
    }

    fn full_into_workspace(
        mut self,
        mut elements: Vec<DecodedElement<'a>>,
        workspace: &mut DecodeWorkspace<'a>,
    ) -> DecodeResult<'a> {
        let defaults = self.default_records();
        resolve::resolve_xxx(self.db, &mut self.items);
        project_reusing(self.db, &mut self.items, defaults, &mut elements);
        let result = DecodeResult {
            vin: self.vin,
            wmi: self.wmi,
            descriptor: self.descriptor,
            model_year: self.model_year,
            error_codes: self.error_codes,
            check_digit_valid: self.check_digit_valid,
            corrected_vin: self.corrected_vin,
            elements,
        };
        workspace.put_items(self.items);
        result
    }

    fn flat(mut self) -> FlatResult<'a> {
        self.materialize_defaults();
        resolve::resolve_xxx(self.db, &mut self.items);
        let mut values: Vec<_> = self
            .items
            .into_iter()
            .filter_map(|it| {
                let e = self.db.element_by_id(it.element_id)?;
                public_decode(self.db, e)?;
                Some((
                    tables::group_rank(self.db.s(e.groupname.to_native())),
                    it.element_id,
                    self.db.s(e.name.to_native()),
                    scrub(it.value),
                ))
            })
            .collect();
        // Stable within an element: repeated notes keep their original order.
        values.sort_by_key(|&(rank, id, _, _)| (rank, id));
        let values = values
            .into_iter()
            .map(|(_, id, name, value)| (name, id, value));
        let attributes = if self.db.unique_public_variables() {
            flatten_unique_variables(values)
        } else {
            flatten_values(values)
        };
        FlatResult {
            vin: self.vin,
            wmi: self.wmi,
            descriptor: self.descriptor,
            model_year: self.model_year,
            error_codes: self.error_codes,
            check_digit_valid: self.check_digit_valid,
            corrected_vin: self.corrected_vin,
            attributes,
        }
    }
}

fn decode_items<'a>(
    db: &'a Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
    caller_year: Option<i32>,
) -> RawResult<'a> {
    decode_items_with_pruning(db, input, now_micros, current_year, caller_year, true)
}

fn decode_items_with_pruning<'a>(
    db: &'a Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
    caller_year: Option<i32>,
    prune: bool,
) -> RawResult<'a> {
    let mut workspace = DecodeWorkspace::default();
    decode_items_with_pruning_and_buffers_workspace(
        db,
        input,
        now_micros,
        current_year,
        caller_year,
        prune,
        String::new(),
        String::new(),
        String::new(),
        &mut workspace,
    )
}

#[allow(clippy::too_many_arguments)]
fn decode_items_with_pruning_and_buffers_workspace<'a>(
    db: &'a Db,
    input: &str,
    now_micros: i64,
    current_year: i32,
    caller_year: Option<i32>,
    prune: bool,
    vin: String,
    wmi: String,
    descriptor: String,
    workspace: &mut DecodeWorkspace<'a>,
) -> RawResult<'a> {
    let vin = sanitize_into(input, vin);
    let var_wmi = sanitized_wmi_into(&vin, wmi);
    let descriptor = sanitized_descriptor_into(&vin, descriptor);
    let var_keys = decode::build_var_keys_stack(&vin);
    let context = VinPassContext::new(db, &vin, &var_wmi, now_micros);

    let v_limit = current_year + 2;
    let plan = year::resolve_years_with_wmi(&vin, db, context.any_wmi, current_year);

    // Pass 1 (descriptor/dmy) is permanently dead in the proc — skipped here.
    let mut passes = workspace.take_passes();
    // Shared by every pass of this decode: the key scan does not vary with year.
    let mut scan = decode::PatternScan::default();
    let mut model_year_source = std::borrow::Cow::Borrowed(decode::DEFAULT_MODEL_YEAR_SOURCE);
    let mut do3and4 = true;

    // Pass 2: caller year, only when in [1980, v_limit] and not already a candidate.
    if let Some(yc) = caller_year {
        if (1980..=v_limit).contains(&yc) {
            if Some(yc) == plan.rmy || Some(yc) == plan.omy {
                // SQL sets @do3and4 = 1 here; it is already 1, so this arm is a
                // no-op. Kept explicit to mirror spvindecode.sql line-for-line.
            } else {
                model_year_source = std::borrow::Cow::Owned(yc.to_string());
                let p = run_pass(
                    db,
                    &vin,
                    &var_wmi,
                    var_keys.as_str(),
                    &descriptor,
                    2,
                    Some(yc),
                    model_year_source.as_ref(),
                    true,
                    true,
                    &mut scan,
                    i32::MIN,
                    &context,
                    workspace,
                )
                .expect("the first pass is never pruned");
                do3and4 = p.codes.contains(&8) && plan.rmy.is_some();
                passes.push(p);
            }
        }
    }

    if do3and4 {
        // Pass 3: rmy.
        let e12 = caller_year.is_some() && plan.rmy.is_some() && caller_year != plan.rmy;
        let floor = if prune {
            best_error_value(&passes)
        } else {
            i32::MIN
        };
        passes.extend(run_pass(
            db,
            &vin,
            &var_wmi,
            var_keys.as_str(),
            &descriptor,
            3,
            plan.rmy,
            model_year_source.as_ref(),
            plan.conclusive,
            e12,
            &mut scan,
            floor,
            &context,
            workspace,
        ));
        // Pass 4: omy (only when inconclusive).
        if let Some(omy) = plan.omy {
            let e12 = caller_year.is_some() && caller_year != Some(omy);
            let floor = if prune {
                best_error_value(&passes)
            } else {
                i32::MIN
            };
            passes.extend(run_pass(
                db,
                &vin,
                &var_wmi,
                var_keys.as_str(),
                &descriptor,
                4,
                Some(omy),
                model_year_source.as_ref(),
                plan.conclusive,
                e12,
                &mut scan,
                floor,
                &context,
                workspace,
            ));
        }
    }

    let best_id = best_pass(&passes, db, caller_year);
    let best_index = passes
        .iter()
        .position(|pass| pass.id == best_id)
        .expect("at least one pass ran");
    let best = passes.swap_remove(best_index);
    for pass in passes.drain(..) {
        workspace.put_items(pass.items);
    }
    workspace.put_passes(passes);

    let mut items = best.items;
    // Which defaults apply is fixed by the items before QC filtering.
    let defaults = best.defaults.map(|(vehicle_type, at)| Defaults {
        vehicle_type,
        mask: decode::default_mask(db, vehicle_type, &items[..at]),
        at: at - items[..at].iter().filter(|it| it.to_be_qced).count(),
    });
    // QC filtering belongs after scoring, before every output shape.
    items.retain(|it| !it.to_be_qced);

    RawResult {
        db,
        vin,
        wmi: var_wmi,
        descriptor,
        model_year: best.model_year,
        error_codes: best.codes,
        check_digit_valid: best.check_digit_valid,
        corrected_vin: best.corrected_vin,
        items,
        defaults,
    }
}

/// ErrorValue is the first score component, before element weights and the
/// caller-year bonus. Only a strictly worse upper bound permits pruning.
fn best_error_value(passes: &[Pass]) -> i32 {
    passes
        .iter()
        .map(|p| p.codes.iter().map(|c| tables::errorcode_weight(*c)).sum())
        .max()
        .unwrap_or(i32::MIN)
}

/// Run one `spvindecode_core` pass and append its corrections.
#[allow(clippy::too_many_arguments)]
fn run_pass<'a>(
    db: &'a Db,
    vin: &str,
    var_wmi: &str,
    var_keys: &str,
    descriptor: &str,
    id: i32,
    model_year: Option<i32>,
    model_year_source: &str,
    conclusive: bool,
    error12: bool,
    scan: &mut decode::PatternScan,
    error_floor: i32,
    context: &VinPassContext<'a>,
    workspace: &mut DecodeWorkspace<'a>,
) -> Option<Pass<'a>> {
    // No WMI means error 7; no PatternId-bearing item means error 8. All
    // additional code weights are non-positive, so this bounds either score.
    let ceiling = tables::errorcode_weight(7).max(tables::errorcode_weight(8));
    if error_floor > ceiling && !db.may_have_pattern_rows_for(context.public_wmi, model_year) {
        return None;
    }
    let core = decode::decode_core_into(
        db,
        var_wmi,
        context.public_wmi,
        var_keys,
        model_year,
        model_year_source,
        scan,
        workspace.take_items(),
    );
    if error_floor > ceiling
        && !core
            .items
            .iter()
            .any(|it| it.pattern_id != tables::NULL_I32)
    {
        workspace.put_items(core.items);
        return None;
    }
    let err = errors::compute_errors_with_context(
        db,
        vin,
        var_wmi,
        &core,
        model_year,
        error12,
        conclusive,
        context.is_car_mpv_lt,
        context.is_vin_exception,
    );

    let defaults = core
        .defaults
        .map(|vehicle_type| (vehicle_type, core.items.len()));
    let mut items = core.items;
    let (codes_csv, error_text) = correction_text(db, &err);
    append_correction(&mut items, 142, err.corrected_vin.clone());
    append_correction(&mut items, 143, codes_csv);
    append_correction(&mut items, 144, err.error_bytes);
    append_correction(&mut items, 156, err.additional_info);
    append_correction(&mut items, 191, error_text);
    append_correction(&mut items, 196, descriptor.to_string());

    Some(Pass {
        id,
        model_year,
        items,
        defaults,
        codes: err.codes,
        corrected_vin: err.corrected_vin,
        check_digit_valid: err.check_digit_valid,
    })
}

/// Pick the best pass by the `x` scoring table: ErrorValue desc, ElementsWeight
/// desc, Patterns desc, ModelYear desc (NULLs last), then lowest pass id.
fn best_pass(passes: &[Pass], db: &Db, caller_year: Option<i32>) -> i32 {
    if let [pass] = passes {
        return pass.id;
    }
    // Score each candidate once, without allocating a collection of scores.
    passes
        .iter()
        .map(|p| (p.id, score(p, db, caller_year)))
        .max_by(|(ida, sa), (idb, sb)| {
            // a is "greater" (preferred) when its tuple ranks higher.
            sa.0.cmp(&sb.0)
                .then(sa.1.cmp(&sb.1))
                .then(sa.2.cmp(&sb.2))
                .then(cmp_year_nulls_last(sa.3, sb.3))
                .then(idb.cmp(ida)) // lower id wins ties
        })
        .map(|(id, _)| id)
        .unwrap_or(0)
}

/// (ErrorValue, ElementsWeight, Patterns, ModelYear+bonus) for a pass.
type Score = (i32, i32, i32, Option<i32>);

fn score(pass: &Pass, db: &Db, caller_year: Option<i32>) -> Score {
    let error_value: i32 = pass
        .codes
        .iter()
        .map(|c| tables::errorcode_weight(*c))
        .sum();

    let mut weighted = hash::ElementSet::default();
    let item_weight: i32 = pass
        .items
        .iter()
        .filter(|it| !it.value.is_empty() && weighted.insert(it.element_id))
        .filter_map(|it| db.element_by_id(it.element_id))
        .map(|e| e.weight.to_native())
        .filter(|w| *w != tables::NULL_I32)
        .sum();
    // Deferred defaults: each applying row has a non-empty value and an element
    // no item shares, so it adds exactly its element's weight.
    let default_weight: i32 = pass.defaults.map_or(0, |(vehicle_type, at)| {
        let mask = decode::default_mask(db, vehicle_type, &pass.items[..at]);
        db.default_templates_for(vehicle_type)
            .iter()
            .enumerate()
            .filter(|(i, _)| mask >> i & 1 != 0)
            .map(|(_, dv)| dv.weight)
            .sum()
    });
    let elements_weight = item_weight + default_weight;

    let patterns = pass
        .items
        .iter()
        .filter(|it| {
            matches!(
                it.source.as_ref(),
                "Pattern" | "EngineModelPattern" | "Formula Pattern"
            ) && !it.value.is_empty()
                && it.value != "Not Applicable"
        })
        .count() as i32;

    let model_year = pass
        .items
        .iter()
        .find(|it| it.element_id == 29)
        .and_then(|it| it.value.parse::<i32>().ok())
        .map(|y| y + if caller_year == Some(y) { 10000 } else { 0 });

    (error_value, elements_weight, patterns, model_year)
}

/// DESC ordering with NULLs last: `Some` always beats `None`.
fn cmp_year_nulls_last(a: Option<i32>, b: Option<i32>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn projection_order(db: &Db, items: &[decode::DecodingItem<'_>]) -> Vec<(u32, usize)> {
    let mut order = Vec::new();
    fill_projection_order(&mut order, db, items, None);
    order
}

/// Output order as `(_, index)`; an index past `items` names default record
/// `index - items.len()`. Default elements never repeat an item's element, so
/// their unique keys place them without any tie.
fn fill_projection_order(
    order: &mut Vec<(u32, usize)>,
    db: &Db,
    items: &[decode::DecodingItem<'_>],
    defaults: DefaultRecords,
) {
    let (records, mask) = defaults.unwrap_or((&[], 0));
    let applied = || {
        records
            .iter()
            .enumerate()
            .filter(move |(i, _)| mask >> i & 1 != 0)
            .filter_map(|(i, record)| Some((items.len() + i, record.as_ref()?)))
    };
    order.clear();
    // Each projected element has a fixed output rank. When no element repeats,
    // marking ranks in a bitmap and reading it back in order is the sort.
    const RANK_WORDS: usize = 4;
    let mut ranked = [0u64; RANK_WORDS];
    let mut item_at = [0u16; RANK_WORDS * 64];
    let ranks = db.output_ranks();
    let mut place = |index: usize, rank: u16| {
        if rank == u16::MAX {
            return true;
        }
        let (word, bit) = (usize::from(rank / 64), rank % 64);
        if word >= RANK_WORDS || ranked[word] >> bit & 1 != 0 {
            return false;
        }
        ranked[word] |= 1 << bit;
        item_at[usize::from(rank)] = index as u16;
        true
    };
    let placed = items.len() + records.len() <= usize::from(u16::MAX)
        && items.iter().enumerate().all(|(index, it)| {
            let rank = usize::try_from(it.element_id)
                .ok()
                .and_then(|id| ranks.get(id).copied())
                .unwrap_or(u16::MAX);
            place(index, rank)
        })
        && applied().all(|(index, &(rank, _))| place(index, rank));
    if placed {
        for (word, mut bits) in ranked.into_iter().enumerate() {
            while bits != 0 {
                let rank = word * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                order.push((rank as u32, usize::from(item_at[rank])));
            }
        }
        return;
    }
    // Repeated elements (notes) keep insertion order: sort (key, item index).
    order.extend(
        items
            .iter()
            .enumerate()
            .map(|(index, it)| (index, it.element_id))
            .chain(applied().map(|(index, (_, record))| (index, record.element_id)))
            .filter_map(|(index, id)| Some((db.output_sort_key(id)?, index))),
    );
    // One u64 comparison orders (key, index) exactly; item counts fit in 32 bits.
    order.sort_unstable_by_key(|&(key, index)| u64::from(key) << 32 | index as u64);
}

const MAX_PROJECTION_SCRATCH_ENTRIES: usize = 256;

thread_local! {
    static PROJECTION_ORDER_SCRATCH: std::cell::RefCell<Vec<(u32, usize)>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

struct ProjectionOrderScratch {
    order: Vec<(u32, usize)>,
}

impl ProjectionOrderScratch {
    /// Take ownership while projecting, so nested decoding can use a fallback
    /// without holding a `RefCell` borrow. Drop returns only bounded capacity.
    fn take() -> Self {
        let order = PROJECTION_ORDER_SCRATCH
            .try_with(|scratch| {
                scratch
                    .try_borrow_mut()
                    .map(|mut scratch| std::mem::take(&mut *scratch))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        Self { order }
    }
}

impl std::ops::Deref for ProjectionOrderScratch {
    type Target = Vec<(u32, usize)>;

    fn deref(&self) -> &Self::Target {
        &self.order
    }
}

impl std::ops::DerefMut for ProjectionOrderScratch {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.order
    }
}

impl Drop for ProjectionOrderScratch {
    fn drop(&mut self) {
        self.order.clear();
        if self.order.capacity() > MAX_PROJECTION_SCRATCH_ENTRIES {
            self.order = Vec::new();
        }
        let _ = PROJECTION_ORDER_SCRATCH.try_with(|scratch| {
            if let Ok(mut slot) = scratch.try_borrow_mut() {
                if slot.capacity() < self.order.capacity() {
                    *slot = std::mem::take(&mut self.order);
                }
            }
        });
    }
}

/// Project the surviving items into output elements (non-empty Decode, public),
/// ordered by the GroupName CASE rank then element id.
fn project<'a>(
    db: &'a Db,
    mut items: Vec<decode::DecodingItem<'a>>,
    defaults: DefaultRecords,
) -> Vec<DecodedElement<'a>> {
    let mut order = ProjectionOrderScratch::take();
    fill_projection_order(&mut order, db, &items, defaults);
    let mut elements: Vec<DecodedElement> = Vec::with_capacity(order.len());
    append_projected(db, &mut items, defaults, &order, &mut elements);
    elements
}

fn append_projected<'a>(
    db: &'a Db,
    items: &mut [decode::DecodingItem<'a>],
    defaults: DefaultRecords,
    order: &[(u32, usize)],
    elements: &mut Vec<DecodedElement<'a>>,
) {
    let records = defaults.map_or(&[][..], |(records, _)| records);
    // Write each 224-byte record straight into reserved capacity; `push`
    // builds it on the stack and copies it in.
    elements.reserve(order.len());
    let spare = elements.spare_capacity_mut();
    let mut written = 0;
    let projection_meta = db.projection_meta_lookup();
    for (_, index) in order.iter().copied() {
        let Some(it) = items.get_mut(index) else {
            if let Some((_, record)) = &records[index - items.len()] {
                spare[written].write(record.clone());
                written += 1;
            }
            continue;
        };
        let Some(meta) = projection_meta(it.element_id) else {
            continue;
        };
        spare[written].write(DecodedElement {
            group_name: &meta.group_name,
            variable: &meta.variable,
            value: scrub_value(std::mem::take(&mut it.value)),
            element_id: it.element_id,
            attribute_id: std::mem::take(&mut it.attribute_id),
            code: &meta.code,
            data_type: &meta.data_type,
            decode: &meta.decode,
            source: std::mem::take(&mut it.source),
            pattern_id: opt_i32(it.pattern_id),
            vin_schema_id: opt_i32(it.vin_schema_id),
            keys: std::mem::take(&mut it.keys),
            created_on: opt_i64(it.created_on),
            wmi_id: opt_i32(it.wmi_id),
            to_be_qced: it.to_be_qced,
        });
        written += 1;
    }
    // SAFETY: the first `written` spare slots were initialized above.
    unsafe { elements.set_len(elements.len() + written) };
}

fn project_reusing<'a>(
    db: &'a Db,
    items: &mut [decode::DecodingItem<'a>],
    defaults: DefaultRecords,
    elements: &mut Vec<DecodedElement<'a>>,
) {
    debug_assert!(elements.is_empty());
    let mut order = ProjectionOrderScratch::take();
    fill_projection_order(&mut order, db, items, defaults);
    elements.reserve_exact(order.len());
    append_projected(db, items, defaults, &order, elements);
}

fn error_codes_csv(codes: &[i32]) -> String {
    let mut csv = String::new();
    for (index, code) in codes.iter().enumerate() {
        if index > 0 {
            csv.push(',');
        }
        let _ = write!(csv, "{code}");
    }
    csv
}

/// Build the element-191 error text: error-code names joined by `; `.
fn error_messages(db: &Db, err: &errors::ErrorState) -> String {
    // Loop-invariant: with no tag the old per-iteration `break` left the buffer
    // empty on the first pass, so bail here for the same "".
    let Some(t) = tables::element_lookup_tag(143) else {
        return String::new();
    };
    // Push straight into one buffer (`; ` separators) instead of a Vec<String> +
    // per-name owned copies + join — same bytes, far fewer allocations.
    let mut out = String::new();
    let mut first = true;
    for &code in &err.codes {
        let Some(name) = db.lookup(t, code) else {
            continue;
        };
        // `; ` before every emitted part but the first — exactly `parts.join("; ")`
        // even when a part trims to empty (so it never collapses a separator).
        if !first {
            out.push_str("; ");
        }
        first = false;
        out.push_str(name.trim());
        if err.is_off_road && code == 1 {
            out.push_str(
                " NOTE: Disregard if this is an off-road vehicle PIN, as check digit calculation may not be accurate.",
            );
        }
        if err.is_vin_exception && code == 0 {
            out.push_str(
                " NOTE: Check Digit Exception - The check digit was given an exception based on data from the OEM indicating an error on production.",
            );
        }
    }
    // `left(errorMessages, 500)` counts CHARACTERS, not bytes; multi-byte chars
    // (e.g. the en-dash in the code-10 message) must not be split mid-codepoint.
    if let Some((byte, _)) = out.char_indices().nth(500) {
        out.truncate(byte);
    }
    out
}

fn correction_key(err: &errors::ErrorState) -> Option<usize> {
    let mut mask = 0_usize;
    let mut previous = None;
    for &code in &err.codes {
        if previous.is_some_and(|prior| prior >= code) {
            return None;
        }
        previous = Some(code);
        let bit = match code {
            0..=14 => code as usize,
            400 => 15,
            _ => return None,
        };
        mask |= 1 << bit;
    }
    let off_road = usize::from(err.is_off_road && mask & (1 << 1) != 0);
    let vin_exception = usize::from(err.is_vin_exception && mask & 1 != 0);
    Some(mask | off_road << 16 | vin_exception << 17)
}

fn correction_text<'a>(
    db: &'a Db,
    err: &errors::ErrorState,
) -> (std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>) {
    use std::borrow::Cow;

    let Some(key) = correction_key(err) else {
        return (
            Cow::Owned(error_codes_csv(&err.codes)),
            Cow::Owned(error_messages(db, err)),
        );
    };
    let cached = db.correction_text(key, || crate::db::CorrectionText {
        codes: error_codes_csv(&err.codes).into_boxed_str(),
        messages: error_messages(db, err).into_boxed_str(),
    });
    (
        Cow::Borrowed(cached.codes.as_ref()),
        Cow::Borrowed(cached.messages.as_ref()),
    )
}

fn append_correction<'a>(
    items: &mut Vec<decode::DecodingItem<'a>>,
    element_id: i32,
    value: impl Into<std::borrow::Cow<'a, str>>,
) {
    let value = value.into();
    items.push(decode::DecodingItem {
        created_on: tables::NULL_I64,
        pattern_id: tables::NULL_I32,
        keys: std::borrow::Cow::Borrowed(""),
        vin_schema_id: tables::NULL_I32,
        wmi_id: tables::NULL_I32,
        element_id,
        attribute_id: value.clone(),
        value,
        source: std::borrow::Cow::Borrowed("Corrections"),
        priority: 999,
        to_be_qced: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> &'static Db {
        // Raw accessor: these tests check `is_loaded()` and skip on the
        // placeholder instead of hitting `embedded()`'s refusal.
        Db::embedded_raw()
    }

    #[test]
    fn check_digit_helper_still_works() {
        assert_eq!(check_digit("1HGCM82633A004352"), Some('3'));
    }

    #[test]
    fn value_cleanup_preserves_text_and_ownership_across_vector_boundaries() {
        use std::borrow::Cow;
        for offset in 0..65 {
            for marker in ["", "\t", "\r", "\n", "\0", "é", "日本語", "\t\r\n"] {
                let text = format!("{}{marker}value", "a".repeat(offset));
                let expected = text.replace(['\t', '\r', '\n'], " ");
                for value in [Cow::Borrowed(text.as_str()), Cow::Owned(text.clone())] {
                    assert_eq!(scrub_value(value), expected);
                }
                if text == expected {
                    assert!(matches!(
                        scrub_value(Cow::Borrowed(&text)),
                        Cow::Borrowed(_)
                    ));
                }
            }
        }
    }

    #[test]
    fn sanitize_ascii_is_the_old_fast_path() {
        // Byte-identical to the original `input.trim().to_ascii_uppercase()`.
        assert_eq!(sanitize("  1hgcm82633a004352 "), "1HGCM82633A004352");
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn sanitize_maps_non_ascii_to_invalid_ascii() {
        // Every non-ASCII char collapses to a single `&` at its char position, so
        // downstream byte indexing sees one byte per input char.
        assert_eq!(sanitize("AAé"), "AA&");
        assert_eq!(sanitize("1HGCM8263Ł3A00435"), "1HGCM8263&3A00435");
        assert_eq!(sanitize("1HGCM82633A0043é2"), "1HGCM82633A0043&2");
        // A non-ASCII whitespace char maps to `&` (kept), not trimmed away — so it
        // matches decoding the `&`-substituted string exactly.
        assert_eq!(sanitize("\u{00a0}AB\u{00a0}"), "&AB&");
    }

    #[test]
    fn sanitized_descriptor_matches_public_descriptor() {
        for input in [
            "1HGCM82633A004352",
            "1F9TC25FTAB123456",
            "short",
            "",
            "  1hgcm82633a004352  ",
            "1HGCM82633A0043é2",
            "1HGCM82633A004352EXTRA",
        ] {
            let vin = sanitize(input);
            assert_eq!(sanitized_descriptor(&vin), vin_descriptor(&vin));
        }
    }

    #[test]
    fn multibyte_input_decodes_like_its_ampersand_twin() {
        // The three field repros that raised PanicException through the wheel.
        let d = db();
        if !d.is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        for (bad, twin) in [
            ("AAé", "AA&"),
            ("1HGCM8263Ł3A00435", "1HGCM8263&3A00435"),
            ("1HGCM82633A0043é2", "1HGCM82633A0043&2"),
        ] {
            let a = decode_with(d, bad, 1_750_000_000_000_000, 2026);
            let b = decode_with(d, twin, 1_750_000_000_000_000, 2026);
            assert_eq!(a, b, "{bad} must decode like {twin}");
        }
    }

    #[test]
    fn canonical_honda_decodes() {
        let d = db();
        if !d.is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        let r = decode_with(d, "1HGCM82633A004352", 1_750_000_000_000_000, 2026);
        let get = |eid: i32| r.elements.iter().find(|e| e.element_id == eid);
        assert_eq!(get(26).map(|e| e.value.as_ref()), Some("HONDA"));
        assert_eq!(get(28).map(|e| e.value.as_ref()), Some("Accord"));
        assert_eq!(r.model_year, Some(2003));
        assert_eq!(get(18).map(|e| e.value.as_ref()), Some("J30A4"));
        assert_eq!(get(39).map(|e| e.value.as_ref()), Some("PASSENGER CAR"));
        assert_eq!(r.error_codes, vec![0]);
    }

    #[test]
    fn long_all_invalid_input_stamps_without_quadratic_blowup() {
        let d = db();
        if !d.is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        // 200 chars invalid at every VIN position (`#` fails every char class —
        // unlike `*`, which vPIC treats as a wildcard). The WMI (`###`) is
        // unregistered, so the corrected VIN starts empty and the C5 scan stamps
        // `!` across the whole input — the path that used to rebuild the Vec once
        // per char (O(n^2)). It must complete and yield exactly one `!` per position.
        let input = "#".repeat(200);
        let r = decode_with(d, &input, 1_750_000_000_000_000, 2026);
        assert_eq!(r.corrected_vin, "!".repeat(200));
        assert!(r.error_codes.contains(&400)); // 400 = invalid character(s)
    }

    #[test]
    fn decode_json_is_valid_and_matches() {
        if !db().is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        let json = decode_json("1HGCM82633A004352", None);
        // Round-trips as valid JSON with the same shape/values as the struct.
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["wmi"], "1HG");
        assert_eq!(v["model_year"], 2003);
        assert_eq!(v["check_digit_valid"], true);
        assert_eq!(v["error_codes"], serde_json::json!([0]));
        let make = v["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["element_id"] == 26)
            .expect("make element");
        assert_eq!(make["value"], "HONDA");
        assert_eq!(make["source"], "pattern - model");
    }

    #[test]
    fn full_result_cows_preserve_json_and_borrow_common_archive_text() {
        use std::borrow::Cow;

        if !db().is_loaded() {
            return;
        }
        let vin = "1HGCM82633A004352";
        let now = 1_750_000_000_000_000;
        let result = decode_at(vin, None, now);
        let struct_json = serde_json::to_value(&result).expect("serialize full result");
        let direct_json: serde_json::Value =
            serde_json::from_str(&decode_json_at(vin, None, now)).expect("direct full JSON");
        assert_eq!(struct_json, direct_json);

        assert!(result
            .elements
            .iter()
            .any(|element| matches!(element.value, Cow::Borrowed(_))));
        assert!(result
            .elements
            .iter()
            .any(|element| matches!(element.attribute_id, Cow::Borrowed(_))));
        assert!(result
            .elements
            .iter()
            .any(|element| matches!(element.keys, Cow::Borrowed(_))));

        let error_code = result
            .elements
            .iter()
            .find(|element| element.element_id == 143)
            .expect("Error Code correction");
        assert!(matches!(error_code.value, Cow::Borrowed("0")));
        assert_eq!(error_code.value, "0");
        let error_text = result
            .elements
            .iter()
            .find(|element| element.element_id == 191)
            .expect("Error Text correction");
        assert!(matches!(error_text.value, Cow::Borrowed(_)));

        let multiple = decode_at("1FMAA50A91A111111", None, now);
        let multiple_codes = multiple
            .elements
            .iter()
            .find(|element| element.element_id == 143)
            .expect("multi-code correction");
        assert_eq!(multiple_codes.value, "0,14");
        assert!(matches!(multiple_codes.value, Cow::Borrowed(_)));

        let default_year = result
            .elements
            .iter()
            .find(|element| element.element_id == 29)
            .expect("default model-year element");
        assert_eq!(default_year.keys, decode::DEFAULT_MODEL_YEAR_SOURCE);
        assert!(matches!(default_year.keys, Cow::Borrowed(_)));

        let caller_year = decode_at(vin, Some(2013), now);
        let caller_year = caller_year
            .elements
            .iter()
            .find(|element| element.element_id == 29)
            .expect("caller model-year element");
        assert_eq!(caller_year.keys, "2013");
        assert!(matches!(caller_year.keys, Cow::Owned(_)));
    }

    #[test]
    fn borrowed_error_message_fast_path_matches_owned_reference_edges() {
        fn reference(db: &Db, err: &errors::ErrorState) -> String {
            let Some(tag) = tables::element_lookup_tag(143) else {
                return String::new();
            };
            let mut parts = Vec::new();
            for &code in &err.codes {
                let Some(name) = db.lookup(tag, code) else {
                    continue;
                };
                let mut part = name.trim().to_string();
                if err.is_off_road && code == 1 {
                    part.push_str(
                        " NOTE: Disregard if this is an off-road vehicle PIN, as check digit calculation may not be accurate.",
                    );
                }
                if err.is_vin_exception && code == 0 {
                    part.push_str(
                        " NOTE: Check Digit Exception - The check digit was given an exception based on data from the OEM indicating an error on production.",
                    );
                }
                parts.push(part);
            }
            errors::trunc500(&parts.join("; "))
        }

        if !db().is_loaded() {
            return;
        }
        for (codes, off_road, vin_exception) in [
            (vec![0], false, false),
            (vec![0, 14], false, false),
            (vec![1], true, false),
            (vec![0], false, true),
            (vec![10], false, false),
            (vec![999], false, false),
            (vec![999, 0, 999], false, false),
            (vec![10; 20], false, false),
        ] {
            let err = errors::ErrorState {
                codes,
                corrected_vin: String::new(),
                error_bytes: String::new(),
                additional_info: String::new(),
                is_off_road: off_road,
                is_vin_exception: vin_exception,
                check_digit_valid: false,
            };
            let expected = reference(db(), &err);
            assert_eq!(error_messages(db(), &err), expected);
            assert_eq!(correction_text(db(), &err).1, expected);
        }
        assert_eq!(error_codes_csv(&[-42]), "-42");
        assert_eq!(error_codes_csv(&[0, 14]), "0,14");
    }

    #[test]
    fn correction_cache_borrows_canonical_keys_and_owns_fallbacks() {
        use std::borrow::Cow;

        if !db().is_loaded() {
            return;
        }
        let state = |codes, off_road, vin_exception| errors::ErrorState {
            codes,
            corrected_vin: String::new(),
            error_bytes: String::new(),
            additional_info: String::new(),
            is_off_road: off_road,
            is_vin_exception: vin_exception,
            check_digit_valid: false,
        };

        let canonical = state(vec![0, 14], false, false);
        let (codes_a, messages_a) = correction_text(db(), &canonical);
        let (codes_b, messages_b) = correction_text(db(), &canonical);
        assert!(matches!(codes_a, Cow::Borrowed(_)));
        assert!(matches!(messages_a, Cow::Borrowed(_)));
        assert!(std::ptr::eq(codes_a.as_ptr(), codes_b.as_ptr()));
        assert!(std::ptr::eq(messages_a.as_ptr(), messages_b.as_ptr()));

        // Flags that cannot affect these codes normalize to the same cache key.
        let irrelevant_flags = state(vec![14], true, true);
        let plain = state(vec![14], false, false);
        let (flagged_codes, flagged_messages) = correction_text(db(), &irrelevant_flags);
        let (plain_codes, plain_messages) = correction_text(db(), &plain);
        assert!(std::ptr::eq(flagged_codes.as_ptr(), plain_codes.as_ptr()));
        assert!(std::ptr::eq(
            flagged_messages.as_ptr(),
            plain_messages.as_ptr()
        ));

        for (code, flag_state) in [(1, (true, false)), (0, (false, true))] {
            let without_note = state(vec![code], false, false);
            let with_note = state(vec![code], flag_state.0, flag_state.1);
            let (_, plain_message) = correction_text(db(), &without_note);
            let (_, noted_message) = correction_text(db(), &with_note);
            assert_ne!(plain_message, noted_message);
            assert!(!std::ptr::eq(
                plain_message.as_ptr(),
                noted_message.as_ptr()
            ));
        }

        for fallback in [
            state(vec![999], false, false),
            state(vec![0, 0], false, false),
            state(vec![14, 0], false, false),
        ] {
            let (codes, messages) = correction_text(db(), &fallback);
            assert!(matches!(codes, Cow::Owned(_)));
            assert!(matches!(messages, Cow::Owned(_)));
        }
    }

    #[test]
    fn full_result_borrows_are_anchored_to_an_external_db() {
        use std::borrow::Cow;

        fn decode_from<'db>(db: &'db Db) -> DecodeResult<'db> {
            db.decode_at("1HGCM82633A004352", None, 1_750_000_000_000_000)
        }

        let bytes = std::fs::read(env!("ULTRAVIN_ARTIFACT")).expect("read built artifact");
        let external = Db::from_bytes(&bytes).expect("load external database");
        let result = decode_from(&external);
        let second = decode_from(&external);
        assert!(result
            .elements
            .iter()
            .any(|element| matches!(element.value, Cow::Borrowed(_))));
        for (left, right) in result.elements.iter().zip(&second.elements) {
            assert_eq!(left, right);
            assert!(std::ptr::eq(left.variable, right.variable));
            assert!(std::ptr::eq(left.decode, right.decode));
        }
        assert_eq!(result.vin, "1HGCM82633A004352");
    }

    #[test]
    fn projection_order_scratch_reuses_normal_capacity_and_drops_outliers() {
        PROJECTION_ORDER_SCRATCH.with(|scratch| scratch.borrow_mut().clear());
        {
            let mut scratch = ProjectionOrderScratch::take();
            scratch.reserve_exact(64);
        }
        let mut reused = ProjectionOrderScratch::take();
        assert!(reused.capacity() >= 64);
        reused.reserve_exact(MAX_PROJECTION_SCRATCH_ENTRIES + 1);
        drop(reused);
        let bounded = ProjectionOrderScratch::take();
        assert!(bounded.capacity() <= MAX_PROJECTION_SCRATCH_ENTRIES);
    }

    #[test]
    fn projection_order_scratch_has_a_reentrant_fallback() {
        PROJECTION_ORDER_SCRATCH.with(|scratch| {
            let _borrowed = scratch.borrow_mut();
            let fallback = ProjectionOrderScratch::take();
            assert_eq!(fallback.capacity(), 0);
        });
    }

    #[test]
    fn decode_flat_matches_flattening_a_full_decode() {
        let d = db();
        if !d.is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        let vin = "1HGCM82633A004352";
        assert_eq!(decode_flat(vin, None), FlatResult::from(decode(vin, None)));
        // The caller year reaches the decode through the flat door too.
        assert_eq!(decode_flat(vin, Some(2013)).model_year, Some(2013));
    }

    #[test]
    fn default_records_match_projected_default_items_across_the_builtin_cover() {
        let Some(db) = Db::try_embedded() else { return };
        assert!(db.defaults_independent());
        for vin in db.cover() {
            for year in [None, Some(2003), Some(2028)] {
                let decode = || decode_items(db, &vin, 1_788_739_200_000_000, 2026, year);
                let mut materialized = decode();
                materialized.materialize_defaults();
                assert_eq!(decode().full(), materialized.full(), "{vin}, {year:?}");
            }
        }
    }

    #[test]
    fn pruning_preserves_best_pass_selection_across_the_builtin_cover() {
        let Some(db) = Db::try_embedded() else { return };
        for vin in db.cover() {
            for year in [None, Some(1980), Some(2003), Some(2028)] {
                let run = |prune| {
                    decode_items_with_pruning(db, &vin, 1_788_739_200_000_000, 2026, year, prune)
                        .full()
                };
                assert_eq!(run(true), run(false), "{vin}, {year:?}");
            }
        }
    }

    #[test]
    fn per_vin_context_preserves_lookup_and_year_semantics() {
        let Some(db) = Db::try_embedded() else { return };
        let cases = [
            // Normal WMI, low-volume six-character WMI, ambiguous model year,
            // unknown WMI, and Unicode sanitization/error input.
            ("1HGCM82633A004352", 2026),
            ("1F9TC25FTAB123456", 2026),
            ("ZZZCM82633A004352", 2050),
            ("1HGCM8263Ł3A00435", 2026),
        ];
        for (input, current_year) in cases {
            let vin = sanitize(input);
            let var_wmi = sanitized_wmi_into(&vin, String::new());
            for now in [i64::MIN, 1_788_739_200_000_000, i64::MAX] {
                let context = VinPassContext::new(db, &vin, &var_wmi, now);
                assert_eq!(
                    context.any_wmi.map(|w| w as *const _),
                    db.wmi_any(&var_wmi).map(|w| w as *const _),
                    "any-WMI row changed for {input:?}"
                );
                assert_eq!(
                    context.public_wmi.map(|w| w as *const _),
                    db.wmi_by_str(&var_wmi, now).map(|w| w as *const _),
                    "publication gate changed for {input:?} at {now}"
                );
                assert_eq!(
                    year::resolve_years_with_wmi(&vin, db, context.any_wmi, current_year),
                    year::resolve_years(&vin, &var_wmi, db, current_year),
                    "year plan changed for {input:?}"
                );
            }
            let now = 1_788_739_200_000_000;
            let context = VinPassContext::new(db, &vin, &var_wmi, now);
            let plan = year::resolve_years(&vin, &var_wmi, db, current_year);
            let var_keys = decode::build_var_keys_stack(&vin);
            for model_year in [None, plan.rmy, plan.omy] {
                let core = decode::decode_core_into(
                    db,
                    &var_wmi,
                    context.public_wmi,
                    var_keys.as_str(),
                    model_year,
                    decode::DEFAULT_MODEL_YEAR_SOURCE,
                    &mut decode::PatternScan::default(),
                    Vec::new(),
                );
                for error12 in [false, true] {
                    assert_eq!(
                        errors::compute_errors_with_context(
                            db,
                            &vin,
                            &var_wmi,
                            &core,
                            model_year,
                            error12,
                            plan.conclusive,
                            context.is_car_mpv_lt,
                            context.is_vin_exception,
                        ),
                        errors::compute_errors(
                            db,
                            &vin,
                            &var_wmi,
                            &core,
                            model_year,
                            error12,
                            plan.conclusive,
                        ),
                        "validation state changed for {input:?}, {model_year:?}, error12={error12}"
                    );
                }
            }
            // Caller years never enter the context and therefore cannot alter
            // any invariant reused by pass 2/3/4.
            for caller_year in [None, Some(1979), Some(1995), Some(2028)] {
                let result =
                    decode_full(db, input, 1_788_739_200_000_000, current_year, caller_year);
                assert_eq!(result.vin, vin, "caller year {caller_year:?}");
                assert_eq!(result.wmi, var_wmi, "caller year {caller_year:?}");
            }
        }
    }

    #[test]
    fn a_patternless_pass_can_be_pruned_only_below_a_strict_score_floor() {
        let Some(db) = Db::try_embedded() else { return };
        for code in (0..=14).chain([400]) {
            assert!(
                tables::errorcode_weight(code) <= 0,
                "pruning relies on non-positive error weights"
            );
        }
        let mut workspace = DecodeWorkspace::default();
        let context = VinPassContext::new(db, "", "", 1_788_739_200_000_000);
        let mut run = |floor| {
            run_pass(
                db,
                "",
                "",
                "",
                "",
                3,
                None,
                "***X*|Y",
                true,
                false,
                &mut decode::PatternScan::default(),
                floor,
                &context,
                &mut workspace,
            )
        };
        assert!(run(i32::MIN).is_some());
        let ceiling = tables::errorcode_weight(7).max(tables::errorcode_weight(8));
        assert!(
            run(ceiling).is_some(),
            "ties still require the remaining score components"
        );
        assert!(run(ceiling + 1).is_none());
    }

    #[test]
    fn adjacent_flat_groups_preserve_first_values_and_repeated_notes() {
        let note = tables::EXEMPT_ELEMENTS[0];
        let rows = vec![
            ("Note", note, "".to_string()),
            ("Note", note, "later note".to_string()),
            ("Make", 26, "".to_string()),
            ("Make", 26, "later make".to_string()),
            ("Model", 28, "example".to_string()),
        ];
        assert_eq!(
            flatten_unique_variables(rows.clone().into_iter()),
            flatten_values(rows.into_iter())
        );
        assert!(flatten_unique_variables(std::iter::empty()).is_empty());
    }

    #[test]
    fn direct_flat_projection_preserves_full_output_order_and_values() {
        let Some(db) = Db::try_embedded() else { return };
        for vin in [
            "",
            "nope",
            "1HGCM82633A004352",
            "1FTFW1ET5DFC10312",
            "ZZZCM82633A004352",
            "1HGCM8263Ł3A00435",
            "1HGCM826?3A004352",
        ] {
            for year in [
                None,
                Some(1979),
                Some(1980),
                Some(2013),
                Some(2028),
                Some(2029),
            ] {
                let raw = decode_items(db, vin, 1_788_739_200_000_000, 2026, year);
                let full = decode_full(db, vin, 1_788_739_200_000_000, 2026, year);
                let expected = FlatResult::from(full);
                let actual = raw.flat();
                assert_eq!(actual, expected, "{vin:?}, {year:?}");
                assert_eq!(
                    serde_json::to_string(&actual).unwrap(),
                    serde_json::to_string(&expected).unwrap()
                );
            }
        }
    }

    #[test]
    fn flat_collapses_elements_and_keeps_notes_as_lists() {
        let d = db();
        if !d.is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        let full = decode_with(d, "1HGCM82633A004352", 1_750_000_000_000_000, 2026);
        let notes: Vec<&str> = multi_valued_variables(d);
        let flat = FlatResult::from(full.clone());

        assert_eq!(flat.vin, full.vin);
        assert_eq!(flat.model_year, full.model_year);
        // Every element's variable is present exactly once, in element order.
        let mut expected: Vec<&str> = Vec::new();
        for e in &full.elements {
            if !expected.contains(&e.variable) {
                expected.push(e.variable);
            }
        }
        let got: Vec<&str> = flat.attributes.iter().map(|(k, _)| *k).collect();
        assert_eq!(got, expected);
        // The exempt note elements are lists even at length one; nothing else is.
        for (name, value) in &flat.attributes {
            match value {
                FlatValue::Many(_) => assert!(notes.contains(name), "{name} should not be a list"),
                FlatValue::One(_) => assert!(!notes.contains(name), "{name} should be a list"),
            }
        }
        let make = flat.attributes.iter().find(|(k, _)| *k == "Make");
        assert_eq!(make.map(|(_, v)| v), Some(&FlatValue::One("HONDA".into())));
    }

    #[test]
    fn flat_json_is_an_object_keyed_by_variable() {
        if !db().is_loaded() {
            return;
        }
        let v: serde_json::Value =
            serde_json::from_str(&decode_json_flat("1HGCM82633A004352", None)).expect("valid JSON");
        assert_eq!(v["model_year"], 2003);
        assert_eq!(v["attributes"]["Make"], "HONDA");
        assert_eq!(v["attributes"]["Model"], "Accord");
        assert!(v["attributes"].get("elements").is_none());
    }

    #[test]
    fn decode_batch_json_is_an_array() {
        if !db().is_loaded() {
            return;
        }
        let json = decode_batch_json(
            &[
                "1HGCM82633A004352".to_string(),
                "SAL00000000000000".to_string(),
            ],
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["wmi"], "1HG");
    }

    #[test]
    fn batch_reordering_and_json_framing_preserve_input_order() {
        if !db().is_loaded() {
            return;
        }
        let vins = ["1HGCM82633A004352", "1FTFW1ET5DFC10312"];
        let inputs: Vec<String> = (0..2_051).map(|i| vins[i % vins.len()].into()).collect();
        let years: Vec<Option<i32>> = (0..inputs.len())
            .map(|i| (i % 3 == 0).then_some(1995))
            .collect();
        let now = 1_750_000_000_000_000;
        let expected: Vec<_> = inputs
            .iter()
            .zip(&years)
            .map(|(vin, year)| decode_json_flat_at(vin, *year, now))
            .collect();

        assert_eq!(
            decode_batch_json_flat_at(&inputs, Some(&years), now),
            format!("[{}]", expected.join(","))
        );
        let batch = decode_batch_flat_at(&inputs, Some(&years), now);
        assert_eq!(batch.len(), inputs.len());
        assert!(batch
            .iter()
            .zip(&inputs)
            .all(|(result, vin)| &result.vin == vin));
    }

    #[test]
    fn managed_full_and_flat_batches_match_legacy_results() {
        if !db().is_loaded() {
            return;
        }
        let samples = ["1HGCM82633A004352", "1FTFW1ET5DFC10312"];
        let inputs: Vec<String> = (0..600)
            .map(|index| samples[index % samples.len()].to_string())
            .collect();
        let years: Vec<Option<i32>> = (0..inputs.len())
            .map(|index| (index % 3 == 0).then_some(1995))
            .collect();
        let now = 1_750_000_000_000_000;

        let full = db().decode_batch_at(&inputs, Some(&years), now);
        let managed_full = db().decode_batch_managed_at(&inputs, Some(&years), now);
        assert_eq!(managed_full, full);

        let flat = db().decode_batch_flat_at(&inputs, Some(&years), now);
        let managed_flat = db().decode_batch_flat_managed_at(&inputs, Some(&years), now);
        assert_eq!(managed_flat, flat);
    }

    #[test]
    fn private_calibration_scope_keeps_per_vin_batch_work_serial() {
        use std::collections::HashSet;
        use std::sync::Mutex;

        if !db().is_loaded() {
            return;
        }
        let inputs: Vec<String> = (0..64).map(|_| "1HGCM82633A004352".to_string()).collect();
        let native_workers = Mutex::new(Vec::new());
        let native_threads = Mutex::new(HashSet::new());
        let json_workers = Mutex::new(Vec::new());
        let json_threads = Mutex::new(HashSet::new());
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("private calibration pool");

        pool.install(|| {
            with_private_calibration_pool_scope(|| {
                drop(batch_at(
                    db(),
                    &inputs,
                    None,
                    1_750_000_000_000_000,
                    |result| {
                        native_workers
                            .lock()
                            .expect("native worker observations")
                            .push(rayon::current_num_threads());
                        native_threads
                            .lock()
                            .expect("native thread observations")
                            .insert(std::thread::current().id());
                        result.full()
                    },
                ));
                drop(batch_json_framed(
                    &inputs,
                    None,
                    1_750_000_000_000_000,
                    |result| {
                        json_workers
                            .lock()
                            .expect("JSONL worker observations")
                            .push(rayon::current_num_threads());
                        json_threads
                            .lock()
                            .expect("JSONL thread observations")
                            .insert(std::thread::current().id());
                        json::encode(result)
                    },
                    true,
                ));
            });
        });

        let native_workers = native_workers.into_inner().expect("native workers");
        let json_workers = json_workers.into_inner().expect("JSONL workers");
        assert_eq!(native_workers.len(), inputs.len());
        assert!(native_workers.iter().all(|workers| *workers == 1));
        assert_eq!(
            native_threads.into_inner().expect("native threads").len(),
            1
        );
        assert_eq!(json_workers.len(), inputs.len());
        assert!(json_workers.iter().all(|workers| *workers == 1));
        assert_eq!(json_threads.into_inner().expect("JSONL threads").len(), 1);
    }

    #[test]
    fn private_calibration_scope_resets_after_panic() {
        let panic = std::panic::catch_unwind(|| {
            with_private_calibration_pool_scope(|| panic!("calibration failed"));
        });
        assert!(panic.is_err());
        assert!(!PRIVATE_CALIBRATION_POOL_DEPTH.with(|depth| depth.get() > 0));
    }

    #[test]
    fn restore_input_order_handles_nontrivial_permutation_cycles() {
        let mut order = [2, 0, 3, 1];
        let mut values = ["c", "a", "d", "b"];
        restore_input_order(&mut order, &mut values);
        assert_eq!(order, [0, 1, 2, 3]);
        assert_eq!(values, ["a", "b", "c", "d"]);
    }

    #[test]
    fn direct_output_slots_preserve_order_and_drop_initialized_values_on_panic() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let inputs: Vec<String> = ["ZZZ", "AAA", "MMM", "BBB"]
            .into_iter()
            .cycle()
            .take(64)
            .map(str::to_owned)
            .collect();
        let output = collect_in_input_order(&inputs, |index| (index, inputs[index].clone()));
        assert!(output
            .iter()
            .enumerate()
            .all(|(index, value)| value.0 == index && value.1 == inputs[index]));

        struct Tracked(Arc<AtomicUsize>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let created = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let created = Arc::clone(&created);
            let dropped = Arc::clone(&dropped);
            move || {
                collect_in_input_order(&inputs, |index| {
                    if index == 17 {
                        panic!("decode failed");
                    }
                    created.fetch_add(1, Ordering::Relaxed);
                    Tracked(Arc::clone(&dropped))
                });
            }
        }));
        assert!(result.is_err());
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            created.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn clock_year_tracks_calendar_boundaries() {
        for (micros, year) in [
            (1_767_225_600_000_000, 2026), // Jan 1
            (1_772_236_800_000_000, 2026), // Feb 28
            (1_772_323_200_000_000, 2026), // Mar 1
            (1_798_675_200_000_000, 2026), // Dec 31
            (1_798_761_600_000_000, 2027), // Jan 1
        ] {
            assert_eq!(current_year_at(micros), year);
        }
    }

    #[test]
    fn provenance_reports_the_actual_embedded_digest() {
        let p = db().provenance();
        assert_eq!(p.artifact_blake3, db().artifact_blake3());
        assert_eq!(p.artifact_blake3.len(), 64);
        assert_eq!(p.decoder_version, env!("CARGO_PKG_VERSION"));
    }
}

#[cfg(test)]
mod clock_tests;
