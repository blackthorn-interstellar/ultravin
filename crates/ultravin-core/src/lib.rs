//! ultravin-core — pure-Rust NHTSA vPIC VIN decoder engine.
//!
//! W1: a working first-pass decode against the embedded rkyv artifact — WMI
//! lookup, schema/pattern matching, layered sources, per-element dedup, element
//! resolution, model year, and the basic error codes. Byte-for-byte parity with
//! the official Postgres `vpic.spvindecode` is the long-term goal; the 4-pass
//! best-of, Conversion/Vehicle-Specs sources, and suggested-VIN are W2.

mod checkdigit;
mod conversion;
pub mod cover;
pub mod db;
mod decode;
mod errors;
pub mod generate;
mod hash;
mod keyspec;
mod matcher;
mod resolve;
pub mod tables;
mod wmi;
mod year;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub use checkdigit::check_digit;
pub use db::Db;
pub use generate::{build_vin, generate, pairwise, seeded, sweep, year_char, Dimension, Filter};
pub use matcher::sqlwild_to_regex;
pub use wmi::{vin_descriptor, vin_wmi};

/// One resolved output element (the 15-column `spvindecode` row).
///
/// The five element-metadata columns (`group_name`/`variable`/`code`/`data_type`/
/// `decode`) borrow the immutable `Db` arena directly (`&'a str`) instead of
/// allocating an owned copy per element per decode — they were the single largest
/// allocation block on the decode path. The item-derived columns
/// (`value`/`attribute_id`/`keys`/`source`) are *moved* out of the decode items in
/// [`project`], not cloned. `source` stays a `Cow` so its overwhelmingly common
/// borrowed-literal form (`"Pattern"`, `"Make"`, …) costs nothing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DecodedElement<'a> {
    pub group_name: &'a str,
    pub variable: &'a str,
    pub value: String,
    pub element_id: i32,
    pub attribute_id: String,
    pub code: &'a str,
    pub data_type: &'a str,
    pub decode: &'a str,
    pub source: std::borrow::Cow<'a, str>,
    pub pattern_id: Option<i32>,
    pub vin_schema_id: Option<i32>,
    pub keys: String,
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
/// pair almost every caller actually reads. Building this costs one map entry per
/// element instead of a 15-key dict, which is the whole point — the marshalling
/// into Python, not the decode, is what it saves.
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
        let mut attributes: Vec<(&'a str, FlatValue)> = Vec::with_capacity(r.elements.len());
        let mut seen: std::collections::HashMap<&'a str, usize, hash::FxBuildHasher> =
            std::collections::HashMap::default();
        for e in r.elements {
            match seen.get(e.variable) {
                Some(&i) => {
                    if let (_, FlatValue::Many(list)) = &mut attributes[i] {
                        list.push(e.value);
                    }
                }
                None => {
                    seen.insert(e.variable, attributes.len());
                    let value = if tables::is_exempt(e.element_id) {
                        FlatValue::Many(vec![e.value])
                    } else {
                        FlatValue::One(e.value)
                    };
                    attributes.push((e.variable, value));
                }
            }
        }
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

/// The element's `Decode` text when it is one [`project`] emits, `None` when the
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
    if v.bytes().any(|b| matches!(b, b'\t' | b'\r' | b'\n')) {
        v.replace(['\t', '\r', '\n'], " ")
    } else {
        v.into_owned()
    }
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

/// The current model year by the system clock, as the decoder reckons it.
pub fn current_year() -> i32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    epoch_to_year(secs)
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
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    decode_full(
        Db::embedded(),
        input,
        secs * 1_000_000,
        epoch_to_year(secs),
        year,
    )
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
    batch(inputs, years, |r| r)
}

/// [`decode_batch`] with the [`FlatResult`] shape. Flattening runs inside the
/// parallel region, so only the (much smaller) marshalling is left to the caller.
pub fn decode_batch_flat(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
) -> Vec<FlatResult<'static>> {
    batch(inputs, years, FlatResult::from)
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

/// The caller year for input `i`: `years` may be absent entirely, shorter than
/// the inputs, or hold `None` gaps — all mean "no year for this VIN".
fn year_at(years: Option<&[Option<i32>]>, i: usize) -> Option<i32> {
    years.and_then(|ys| ys.get(i)).copied().flatten()
}

/// Shared body of the batch paths: decode every input in parallel over the shared
/// archive, mapped through `shape`. Output order matches `inputs`.
fn batch<T: Send>(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    shape: impl Fn(DecodeResult<'static>) -> T + Sync,
) -> Vec<T> {
    use rayon::prelude::*;

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_micros = secs * 1_000_000;
    let current_year = epoch_to_year(secs);
    let db = Db::embedded();
    batch_pool().install(|| {
        inputs
            .par_iter()
            .enumerate()
            .map(|(i, v)| {
                shape(decode_full(
                    db,
                    v,
                    now_micros,
                    current_year,
                    year_at(years, i),
                ))
            })
            .collect()
    })
}

/// Decode one VIN to a compact JSON object string (same shape as the [`decode`]
/// dict). Serializing in Rust avoids the per-field Python dict construction.
pub fn decode_json(input: &str, year: Option<i32>) -> String {
    serde_json::to_string(&decode(input, year)).expect("DecodeResult is infallibly serializable")
}

/// [`decode_json`] with the [`FlatResult`] shape.
pub fn decode_json_flat(input: &str, year: Option<i32>) -> String {
    serde_json::to_string(&FlatResult::from(decode(input, year)))
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
    batch_json(inputs, years, |r| r)
}

/// [`decode_batch_json`] with the [`FlatResult`] shape.
pub fn decode_batch_json_flat(inputs: &[String], years: Option<&[Option<i32>]>) -> String {
    batch_json(inputs, years, FlatResult::from)
}

/// Shared body of the batch-JSON paths: decode + serialize in parallel through
/// `shape`, then stitch one array serially.
fn batch_json<T: serde::Serialize + Send>(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    shape: impl Fn(DecodeResult<'static>) -> T + Sync,
) -> String {
    use rayon::prelude::*;

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_micros = secs * 1_000_000;
    let current_year = epoch_to_year(secs);
    let db = Db::embedded();
    // Fuse decode + serialize so both happen in parallel; each task yields its
    // object's JSON text.
    let parts: Vec<String> = batch_pool().install(|| {
        inputs
            .par_iter()
            .enumerate()
            .map(|(i, v)| {
                serde_json::to_string(&shape(decode_full(
                    db,
                    v,
                    now_micros,
                    current_year,
                    year_at(years, i),
                )))
                .expect("decode results are infallibly serializable")
            })
            .collect()
    });
    // Stitch the array serially (one pass, pre-sized) — cheap memcpy vs. the
    // parallel decode/serialize above.
    let cap = parts.iter().map(|p| p.len() + 1).sum::<usize>() + 2;
    let mut out = String::with_capacity(cap);
    out.push('[');
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(p);
    }
    out.push(']');
    out
}

/// One decode pass (a single `spvindecode_core` invocation): its items (with the
/// 142/143/144/156/191/196 corrections appended, values still pre-resolution) and
/// the metadata the scorer and result need.
struct Pass {
    id: i32,
    model_year: Option<i32>,
    items: Vec<decode::DecodingItem>,
    codes: Vec<i32>,
    corrected_vin: String,
    check_digit_valid: bool,
}

/// Normalize raw decode input into the byte-safe VIN the engine works on.
///
/// Everything downstream — [`vin_wmi`], [`build_var_keys`]'s `&vin[3..8]`/
/// `&vin[9..17]` slices, the check-digit and error-code scans — indexes the VIN
/// *by byte* on the assumption that one byte is one character. That holds only
/// for ASCII: a multibyte char (`é`, `Ł`, …) puts a UTF-8 char boundary
/// mid-index and panics a byte slice. So map every non-ASCII char to one
/// representative invalid ASCII byte (`&`) here, once, at the single entry seam —
/// downstream byte indexing is then unconditionally safe, and a non-ASCII char
/// decodes exactly as an invalid ASCII `&` would at the same character position.
///
/// The all-ASCII case (every real VIN) keeps the original single-allocation
/// `trim().to_ascii_uppercase()` untouched — no new allocation on the hot path.
/// Non-ASCII input maps *before* trimming so the result is byte-for-byte what
/// decoding the `&`-substituted string would produce: a non-ASCII whitespace
/// char becomes a non-whitespace `&` that is kept, not trimmed away.
fn sanitize(input: &str) -> String {
    if input.is_ascii() {
        return input.trim().to_ascii_uppercase();
    }
    let mapped: String = input
        .chars()
        .map(|c| if c.is_ascii() { c } else { '&' })
        .collect();
    mapped.trim().to_ascii_uppercase()
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
    let vin = sanitize(input);
    let var_wmi = vin_wmi(&vin);
    let descriptor = vin_descriptor(&vin);
    let var_keys = decode::build_var_keys(&vin);

    let v_limit = current_year + 2;
    let plan = year::resolve_years(&vin, &var_wmi, db, current_year);

    // Pass 1 (descriptor/dmy) is permanently dead in the proc — skipped here.
    let mut passes: Vec<Pass> = Vec::new();
    let mut model_year_source = "***X*|Y".to_string();
    let mut do3and4 = true;

    // Pass 2: caller year, only when in [1980, v_limit] and not already a candidate.
    if let Some(yc) = caller_year {
        if (1980..=v_limit).contains(&yc) {
            if Some(yc) == plan.rmy || Some(yc) == plan.omy {
                do3and4 = true;
            } else {
                model_year_source = yc.to_string();
                let p = run_pass(
                    db,
                    &vin,
                    &var_wmi,
                    &var_keys,
                    now_micros,
                    &descriptor,
                    2,
                    Some(yc),
                    &model_year_source,
                    true,
                    true,
                );
                do3and4 = p.codes.contains(&8) && plan.rmy.is_some();
                passes.push(p);
            }
        }
    }

    if do3and4 {
        // Pass 3: rmy.
        let e12 = caller_year.is_some() && plan.rmy.is_some() && caller_year != plan.rmy;
        passes.push(run_pass(
            db,
            &vin,
            &var_wmi,
            &var_keys,
            now_micros,
            &descriptor,
            3,
            plan.rmy,
            &model_year_source,
            plan.conclusive,
            e12,
        ));
        // Pass 4: omy (only when inconclusive).
        if let Some(omy) = plan.omy {
            let e12 = caller_year.is_some() && caller_year != Some(omy);
            passes.push(run_pass(
                db,
                &vin,
                &var_wmi,
                &var_keys,
                now_micros,
                &descriptor,
                4,
                Some(omy),
                &model_year_source,
                plan.conclusive,
                e12,
            ));
        }
    }

    let best_id = best_pass(&passes, db, caller_year);
    let best = passes
        .into_iter()
        .find(|p| p.id == best_id)
        .expect("at least one pass ran");

    let mut items = best.items;
    // QC override + TobeQCed delete (inert with current data) then XXX resolution.
    items.retain(|it| !it.to_be_qced);
    resolve::resolve_xxx(db, &mut items);

    let elements = project(db, items);

    DecodeResult {
        vin,
        wmi: var_wmi,
        descriptor,
        model_year: best.model_year,
        error_codes: best.codes,
        check_digit_valid: best.check_digit_valid,
        corrected_vin: best.corrected_vin,
        elements,
    }
}

/// Run one `spvindecode_core` pass and append its corrections.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    db: &Db,
    vin: &str,
    var_wmi: &str,
    var_keys: &str,
    now_micros: i64,
    descriptor: &str,
    id: i32,
    model_year: Option<i32>,
    model_year_source: &str,
    conclusive: bool,
    error12: bool,
) -> Pass {
    let core = decode::decode_core(
        db,
        var_wmi,
        var_keys,
        model_year,
        model_year_source,
        now_micros,
    );
    let err = errors::compute_errors(db, vin, var_wmi, &core, model_year, error12, conclusive);

    let mut items = core.items;
    let mut codes_csv = String::new();
    for (i, c) in err.codes.iter().enumerate() {
        if i > 0 {
            codes_csv.push(',');
        }
        let _ = write!(codes_csv, "{c}");
    }
    let error_text = error_messages(db, &err);
    append_correction(&mut items, 142, &err.corrected_vin);
    append_correction(&mut items, 143, &codes_csv);
    append_correction(&mut items, 144, &err.error_bytes);
    append_correction(&mut items, 156, &err.additional_info);
    append_correction(&mut items, 191, &error_text);
    append_correction(&mut items, 196, descriptor);

    Pass {
        id,
        model_year,
        items,
        codes: err.codes,
        corrected_vin: err.corrected_vin,
        check_digit_valid: err.check_digit_valid,
    }
}

/// Pick the best pass by the `x` scoring table: ErrorValue desc, ElementsWeight
/// desc, Patterns desc, ModelYear desc (NULLs last), then lowest pass id.
fn best_pass(passes: &[Pass], db: &Db, caller_year: Option<i32>) -> i32 {
    // Score each pass once (max_by would otherwise recompute score — and its
    // per-call IntSet — 2·(n−1) times).
    let scored: Vec<(i32, Score)> = passes
        .iter()
        .map(|p| (p.id, score(p, db, caller_year)))
        .collect();
    scored
        .iter()
        .max_by(|(ida, sa), (idb, sb)| {
            // a is "greater" (preferred) when its tuple ranks higher.
            sa.0.cmp(&sb.0)
                .then(sa.1.cmp(&sb.1))
                .then(sa.2.cmp(&sb.2))
                .then(cmp_year_nulls_last(sa.3, sb.3))
                .then(idb.cmp(ida)) // lower id wins ties
        })
        .map(|(id, _)| *id)
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

    let mut weighted: hash::IntSet<i32> = hash::IntSet::default();
    for it in &pass.items {
        if !it.value.is_empty() {
            weighted.insert(it.element_id);
        }
    }
    let elements_weight: i32 = weighted
        .iter()
        .filter_map(|eid| db.element_by_id(*eid))
        .map(|e| e.weight.to_native())
        .filter(|w| *w != tables::NULL_I32)
        .sum();

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

/// Project the surviving items into output elements (non-empty Decode, public),
/// ordered by the GroupName CASE rank then element id.
fn project(db: &Db, items: Vec<decode::DecodingItem>) -> Vec<DecodedElement<'_>> {
    let mut elements: Vec<DecodedElement> = Vec::with_capacity(items.len());
    for it in items {
        let Some(e) = db.element_by_id(it.element_id) else {
            continue;
        };
        let Some(decode_str) = public_decode(db, e) else {
            continue;
        };
        elements.push(DecodedElement {
            group_name: db.s(e.groupname.to_native()),
            variable: db.s(e.name.to_native()),
            value: scrub(it.value),
            element_id: it.element_id,
            attribute_id: it.attribute_id,
            code: db.s(e.code.to_native()),
            data_type: db.s(e.datatype.to_native()),
            decode: decode_str,
            source: it.source,
            pattern_id: opt_i32(it.pattern_id),
            vin_schema_id: opt_i32(it.vin_schema_id),
            keys: it.keys,
            created_on: opt_i64(it.created_on),
            wmi_id: opt_i32(it.wmi_id),
            to_be_qced: it.to_be_qced,
        });
    }
    // GroupName CASE rank, then element id (the proc leaves intra-group order to
    // the scan; element id is the deterministic, stable secondary key). A cached
    // key keeps `group_rank` to one call per element and sorts in place — no tuple
    // Vec and no second pass to strip the rank. `sort_by_cached_key` is stable, so
    // the few duplicate exempt elements keep insertion order, exactly as before.
    elements.sort_by_cached_key(|e| (tables::group_rank(e.group_name), e.element_id));
    elements
}

/// Build the element-191 error text: error-code names joined by `; `.
fn error_messages(db: &Db, err: &errors::ErrorState) -> String {
    let tag = tables::element_lookup_tag(143);
    // Push straight into one buffer (`; ` separators) instead of a Vec<String> +
    // per-name owned copies + join — same bytes, far fewer allocations.
    let mut out = String::new();
    let mut first = true;
    for &code in &err.codes {
        let Some(t) = tag else { break };
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
    out.chars().take(500).collect()
}

fn append_correction(items: &mut Vec<decode::DecodingItem>, element_id: i32, value: &str) {
    items.push(decode::DecodingItem {
        created_on: tables::NULL_I64,
        pattern_id: tables::NULL_I32,
        keys: String::new(),
        vin_schema_id: tables::NULL_I32,
        wmi_id: tables::NULL_I32,
        element_id,
        attribute_id: value.to_string(),
        value: std::borrow::Cow::Owned(value.to_string()),
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
        assert_eq!(get(26).map(|e| e.value.as_str()), Some("HONDA"));
        assert_eq!(get(28).map(|e| e.value.as_str()), Some("Accord"));
        assert_eq!(r.model_year, Some(2003));
        assert_eq!(get(18).map(|e| e.value.as_str()), Some("J30A4"));
        assert_eq!(get(39).map(|e| e.value.as_str()), Some("PASSENGER CAR"));
        assert_eq!(r.error_codes, vec![0]);
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
}
