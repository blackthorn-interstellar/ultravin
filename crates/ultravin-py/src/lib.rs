//! PyO3 bindings: exposes `ultravin._ultravin` with `decode`/`decode_batch`.
//! All logic lives in `ultravin-core`; this layer only marshals to Python.

use std::cell::RefCell;
use std::path::PathBuf;

use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int32Type, Int64Type};
use arrow_array::{Array, ArrayRef, RecordBatch};
use arrow_schema::{DataType, SchemaRef};
use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use pyo3::IntoPyObjectExt;

use ultravin_core::parquet_io::{
    decode_parquet_to_file, open_chunks, ParquetChunkIter, ParquetError, ParquetOpts,
};
use ultravin_core::{DecodeResult, DecodedElement, FlatResult, FlatValue};

// The decode engine is allocation-bound; a sharded allocator both speeds the
// single-stream malloc path and removes the global-heap-lock contention that was
// capping `decode_batch` scaling across rayon workers. Gated to the arches that
// carry the mimalloc dep (mainstream 64-bit); the exotic cross targets keep the
// system allocator and stay pure-Rust. See Cargo.toml.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

thread_local! {
    /// `element_id -> [group_name, variable, code, data_type, decode]` interned as
    /// `PyString`s. Those five columns are element *metadata* — a pure function of
    /// `element_id` and constant for the life of the interpreter — yet a naïve
    /// marshaller allocates five fresh `PyString`s for every element of every VIN.
    /// Once the decode itself is parallel + cheap, this GIL-serial marshalling is
    /// the batch bottleneck; caching turns ~5×(elements) `PyString` allocations
    /// per VIN into one-time-per-element-id creation plus refcount bumps.
    ///
    /// Subinterpreter safety: the cached `Py<PyString>` are keyed per-thread, not
    /// per-interpreter. This module does NOT declare `Py_mod_multiple_interpreters`,
    /// so CPython refuses to import it under a per-interpreter GIL — the mode where
    /// unsynchronized cross-interpreter refcounting would be UB. Legacy shared-GIL
    /// subinterpreters (`Py_NewInterpreter()`, mod_wsgi) DO import it, and there the
    /// cache hands one interpreter's strings to another: an isolation-contract
    /// violation, accepted knowingly — the GIL serializes the refcounting and the
    /// strings are immutable, so it cannot corrupt memory, and pyo3's own `intern!`
    /// (used throughout `elem_to_dict`) shares strings process-wide the same way,
    /// so keying this cache by interpreter would not make the module clean.
    /// `fork()` gets a fresh process + thread-local, and the batch pool already
    /// re-keys on pid (see `ultravin-core::lib`). If this module ever opts into
    /// multiple-interpreters, this cache MUST be reworked to key by interpreter.
    static META_CACHE: RefCell<Vec<Option<[Py<PyString>; 5]>>> = const { RefCell::new(Vec::new()) };
}

/// The cached five metadata `PyString`s for an element (created + memoized on
/// first sight of its id). They are immutable and content-identical to a fresh
/// `PyString`, so reuse is transparent to callers.
fn meta_strings(py: Python<'_>, e: &DecodedElement<'_>) -> [Py<PyString>; 5] {
    let id = e.element_id;
    // Real element ids are small positives; never grow an unbounded cache on a
    // stray negative id (just build the strings without memoizing).
    if id < 0 {
        return [
            PyString::new(py, e.group_name).unbind(),
            PyString::new(py, e.variable).unbind(),
            PyString::new(py, e.code).unbind(),
            PyString::new(py, e.data_type).unbind(),
            PyString::new(py, e.decode).unbind(),
        ];
    }
    let id = id as usize;
    META_CACHE.with(|c| {
        let mut v = c.borrow_mut();
        if id >= v.len() {
            v.resize_with(id + 1, || None);
        }
        if let Some(cached) = &v[id] {
            return cached.each_ref().map(|p| p.clone_ref(py));
        }
        let arr = [
            PyString::new(py, e.group_name).unbind(),
            PyString::new(py, e.variable).unbind(),
            PyString::new(py, e.code).unbind(),
            PyString::new(py, e.data_type).unbind(),
            PyString::new(py, e.decode).unbind(),
        ];
        let ret = arr.each_ref().map(|p| p.clone_ref(py));
        v[id] = Some(arr);
        ret
    })
}

fn elem_to_dict<'py>(py: Python<'py>, e: &DecodedElement<'_>) -> PyResult<Bound<'py, PyDict>> {
    // `intern!` reuses one cached `PyString` per key per interpreter instead of
    // allocating a fresh key string on every `set_item` — these 15 keys recur for
    // every element of every decode, so this is the bulk of the marshalling cost.
    let d = PyDict::new(py);
    let [group_name, variable, code, data_type, decode] = meta_strings(py, e);
    d.set_item(intern!(py, "group_name"), group_name)?;
    d.set_item(intern!(py, "variable"), variable)?;
    d.set_item(intern!(py, "value"), &e.value)?;
    d.set_item(intern!(py, "element_id"), e.element_id)?;
    d.set_item(intern!(py, "attribute_id"), &e.attribute_id)?;
    d.set_item(intern!(py, "code"), code)?;
    d.set_item(intern!(py, "data_type"), data_type)?;
    d.set_item(intern!(py, "decode"), decode)?;
    d.set_item(intern!(py, "source"), e.source.as_ref())?;
    d.set_item(intern!(py, "pattern_id"), e.pattern_id)?;
    d.set_item(intern!(py, "vin_schema_id"), e.vin_schema_id)?;
    d.set_item(intern!(py, "keys"), &e.keys)?;
    d.set_item(intern!(py, "created_on"), e.created_on)?;
    d.set_item(intern!(py, "wmi_id"), e.wmi_id)?;
    d.set_item(intern!(py, "to_be_qced"), e.to_be_qced)?;
    Ok(d)
}

fn result_to_dict<'py>(py: Python<'py>, r: &DecodeResult<'_>) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item(intern!(py, "vin"), &r.vin)?;
    d.set_item(intern!(py, "wmi"), &r.wmi)?;
    d.set_item(intern!(py, "descriptor"), &r.descriptor)?;
    d.set_item(intern!(py, "model_year"), r.model_year)?;
    d.set_item(intern!(py, "error_codes"), &r.error_codes)?;
    d.set_item(intern!(py, "check_digit_valid"), r.check_digit_valid)?;
    d.set_item(intern!(py, "corrected_vin"), &r.corrected_vin)?;
    // Pre-size the element list (one allocation) instead of grow-by-append.
    let dicts: Vec<Bound<'py, PyDict>> = r
        .elements
        .iter()
        .map(|e| elem_to_dict(py, e))
        .collect::<PyResult<_>>()?;
    d.set_item(intern!(py, "elements"), PyList::new(py, &dicts)?)?;
    Ok(d)
}

/// The `flat=True` shape: header fields, then one `attributes` dict of
/// `variable -> value`. That is ~41 dict stores per VIN instead of the ~615 the
/// `elements` list costs (one 15-key dict per element), which is the bulk of the
/// GIL-serial marshalling — see `docs/BENCHMARKS.md`.
fn flat_to_dict<'py>(py: Python<'py>, r: &FlatResult<'_>) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item(intern!(py, "vin"), &r.vin)?;
    d.set_item(intern!(py, "wmi"), &r.wmi)?;
    d.set_item(intern!(py, "descriptor"), &r.descriptor)?;
    d.set_item(intern!(py, "model_year"), r.model_year)?;
    d.set_item(intern!(py, "error_codes"), &r.error_codes)?;
    d.set_item(intern!(py, "check_digit_valid"), r.check_digit_valid)?;
    d.set_item(intern!(py, "corrected_vin"), &r.corrected_vin)?;
    let attrs = PyDict::new(py);
    for (name, value) in &r.attributes {
        match value {
            FlatValue::One(v) => attrs.set_item(name, v)?,
            FlatValue::Many(vs) => attrs.set_item(name, PyList::new(py, vs)?)?,
        }
    }
    d.set_item(intern!(py, "attributes"), attrs)?;
    Ok(d)
}

/// The optional per-VIN caller years for a batch, validated against the VIN
/// count. vPIC's batch API carries one optional model year per VIN line, so the
/// binding takes a parallel list; a silent zip-truncation would misattribute
/// years, hence the hard length check.
fn check_years(vins: &[String], years: &Option<Vec<Option<i32>>>) -> PyResult<()> {
    if let Some(ys) = years {
        if ys.len() != vins.len() {
            return Err(PyValueError::new_err(format!(
                "years has {} entries but vins has {}; pass one entry (int or None) per VIN",
                ys.len(),
                vins.len()
            )));
        }
    }
    Ok(())
}

/// Decode a single VIN to a dict.
#[pyfunction]
#[pyo3(signature = (vin, *, year = None, flat = false))]
fn decode<'py>(
    py: Python<'py>,
    vin: &str,
    year: Option<i32>,
    flat: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let r = ultravin_core::decode(vin, year);
    if flat {
        flat_to_dict(py, &r.into())
    } else {
        result_to_dict(py, &r)
    }
}

/// Decode a batch of VINs to a list of dicts.
///
/// The decode work runs in parallel with the GIL released; only the final
/// marshalling of results into Python dicts holds the GIL.
#[pyfunction]
#[pyo3(signature = (vins, *, years = None, flat = false))]
fn decode_batch<'py>(
    py: Python<'py>,
    vins: Vec<String>,
    years: Option<Vec<Option<i32>>>,
    flat: bool,
) -> PyResult<Vec<Bound<'py, PyDict>>> {
    check_years(&vins, &years)?;
    let years = years.as_deref();
    if flat {
        // Flattening happens inside the parallel region, so the GIL-held part is
        // only the (much smaller) dict build.
        let results = py.detach(|| ultravin_core::decode_batch_flat(&vins, years));
        return results.iter().map(|r| flat_to_dict(py, r)).collect();
    }
    let results = py.detach(|| ultravin_core::decode_batch(&vins, years));
    results.iter().map(|r| result_to_dict(py, r)).collect()
}

/// Decode a single VIN to a JSON object string (same shape as `decode`).
#[pyfunction]
#[pyo3(signature = (vin, *, year = None, flat = false))]
fn decode_json(vin: &str, year: Option<i32>, flat: bool) -> String {
    if flat {
        ultravin_core::decode_json_flat(vin, year)
    } else {
        ultravin_core::decode_json(vin, year)
    }
}

/// The variable names whose `flat=True` value is always a list.
#[pyfunction]
fn multi_valued() -> Vec<&'static str> {
    ultravin_core::multi_valued_variables(ultravin_core::Db::embedded())
}

/// The static element table: one `(variable, element_id, group_name, code,
/// data_type, decode)` dict per public element, for callers that want to pin to
/// element ids rather than the vPIC variable names (which NHTSA renames between
/// data releases).
#[pyfunction]
fn elements(py: Python<'_>) -> PyResult<Vec<Bound<'_, PyDict>>> {
    let db = ultravin_core::Db::embedded();
    let mut out = Vec::new();
    for e in db.elements() {
        let Some(decode) = ultravin_core::public_decode(db, e) else {
            continue;
        };
        let d = PyDict::new(py);
        d.set_item(intern!(py, "variable"), db.s(e.name.to_native()))?;
        d.set_item(intern!(py, "element_id"), e.id.to_native())?;
        d.set_item(intern!(py, "group_name"), db.s(e.groupname.to_native()))?;
        d.set_item(intern!(py, "code"), db.s(e.code.to_native()))?;
        d.set_item(intern!(py, "data_type"), db.s(e.datatype.to_native()))?;
        d.set_item(intern!(py, "decode"), decode)?;
        out.push(d);
    }
    Ok(out)
}

/// Decode a batch of VINs to a single JSON array string.
///
/// Decode *and* JSON serialization run in parallel with the GIL released; the
/// caller gets back one string (`json.loads` it for a list equal to
/// `decode_batch`). For large batches this is several times faster than
/// `decode_batch`, which must build a ~15-key dict per element under the GIL.
#[pyfunction]
#[pyo3(signature = (vins, *, years = None, flat = false))]
fn decode_batch_json(
    py: Python<'_>,
    vins: Vec<String>,
    years: Option<Vec<Option<i32>>>,
    flat: bool,
) -> PyResult<String> {
    check_years(&vins, &years)?;
    Ok(py.detach(|| {
        let years = years.as_deref();
        if flat {
            ultravin_core::decode_batch_json_flat(&vins, years)
        } else {
            ultravin_core::decode_batch_json(&vins, years)
        }
    }))
}

/// Upper bound on a single `generate` request. A larger `n` is almost certainly a
/// mistake, and left unchecked it drives a multi-terabyte allocation that aborts
/// the process (uncatchable) rather than raising. Ten million VINs is already far
/// past any legitimate fixture.
const GENERATE_MAX: usize = 10_000_000;

/// Generate `n` valid VINs, deterministic for a given `seed`.
///
/// Each VIN comes from a real WMI, a schema that WMI uses, and one of that
/// schema's patterns, so it decodes to real attributes rather than to an
/// unknown-manufacturer error. Filters are conjunctive; `vehicle_type` is a
/// VehicleType row id (2 = passenger car, 7 = MPV) and `year` is the year the
/// VIN decodes to, not merely the character in position 10. Returns fewer than
/// `n` only when the filter matches nothing. Raises `ValueError` when `n`
/// exceeds `GENERATE_MAX`.
#[pyfunction]
#[pyo3(signature = (n, *, seed = 0, wmi = None, make = None, year = None, vehicle_type = None))]
fn generate(
    py: Python<'_>,
    n: usize,
    seed: u64,
    wmi: Option<String>,
    make: Option<String>,
    year: Option<i32>,
    vehicle_type: Option<i32>,
) -> PyResult<Vec<String>> {
    if n > GENERATE_MAX {
        return Err(PyValueError::new_err(format!(
            "n={n} is too large; generate at most {GENERATE_MAX} VINs per call"
        )));
    }
    let filter = ultravin_core::Filter {
        wmi,
        make,
        year,
        vehicle_type,
    };
    Ok(py.detach(|| {
        ultravin_core::generate(
            ultravin_core::Db::embedded(),
            n,
            seed,
            &filter,
            ultravin_core::now_micros(),
            ultravin_core::current_year(),
        )
    }))
}

/// One VIN per row of every requested data dimension — the brute-force list.
///
/// `dimensions` names any of `wmi`, `pattern`, `engine`, `vspec`, `exception`,
/// `default`; omit it for all six. This is large (the `pattern` dimension alone
/// is ~545k VINs), so ask for one dimension at a time unless you want the lot.
#[pyfunction]
#[pyo3(signature = (dimensions = None))]
fn sweep(py: Python<'_>, dimensions: Option<Vec<String>>) -> PyResult<Vec<String>> {
    let dims = match dimensions {
        None => ultravin_core::Dimension::ALL.to_vec(),
        Some(names) => names
            .iter()
            .map(|n| match n.as_str() {
                "wmi" => Ok(ultravin_core::Dimension::Wmi),
                "pattern" => Ok(ultravin_core::Dimension::Pattern),
                "engine" => Ok(ultravin_core::Dimension::Engine),
                "vspec" => Ok(ultravin_core::Dimension::VehicleSpec),
                "exception" => Ok(ultravin_core::Dimension::Exception),
                "default" => Ok(ultravin_core::Dimension::Default),
                other => Err(PyValueError::new_err(format!("unknown dimension: {other}"))),
            })
            .collect::<PyResult<Vec<_>>>()?,
    };
    Ok(py.detach(|| {
        ultravin_core::sweep(
            ultravin_core::Db::embedded(),
            &dims,
            ultravin_core::current_year(),
        )
    }))
}

/// The smallest VIN set that exercises every decode behaviour this data month
/// can reach — computed when the artifact was built, so it costs nothing here.
///
/// Use it as a decoder test corpus: a few hundred VINs that between them touch
/// every resolution rung, error code, conversion and tiebreak the data supports.
#[pyfunction]
fn cover_vins() -> Vec<String> {
    ultravin_core::Db::embedded().cover()
}

/// Every pair of descriptor character-classes each schema can distinguish.
///
/// The full output space cannot be enumerated — elements driven by disjoint
/// descriptor positions vary independently, so their values multiply. This is
/// the strongest coverage that is finite: strength-2 covering arrays, which buy
/// the interactions the decoder's own logic turns on (dedup, tiebreaks, an
/// element read while resolving a sibling) at roughly 3x the row sweep.
#[pyfunction]
#[pyo3(signature = (*, limit = 0))]
fn pairwise(py: Python<'_>, limit: usize) -> Vec<String> {
    py.detach(|| {
        ultravin_core::pairwise(
            ultravin_core::Db::embedded(),
            ultravin_core::current_year(),
            limit,
        )
    })
}

/// Every decoding rule matched *and* every 2-way descriptor interaction covered,
/// in one corpus.
///
/// Each rule's own key seeds a VIN — the positions it pins stay pinned, so the
/// rule is guaranteed to match — and the positions it leaves free are chosen to
/// knock out outstanding class pairs. Cheaper than sweeping and pairing
/// separately, and stronger per VIN: the freed positions make sibling rules
/// co-match instead of being padding chosen so nothing else does.
#[pyfunction]
#[pyo3(signature = (*, limit = 0))]
fn seeded(py: Python<'_>, limit: usize) -> Vec<String> {
    py.detach(|| {
        ultravin_core::seeded(
            ultravin_core::Db::embedded(),
            ultravin_core::current_year(),
            limit,
        )
    })
}

/// `Config` is a caller mistake (unknown column, bad element id) and `Io` is the
/// filesystem talking, so they get the two exceptions Python callers already
/// catch for those things.
fn parquet_err(e: ParquetError) -> PyErr {
    match e {
        ParquetError::Io(m) => PyOSError::new_err(m),
        ParquetError::Config(m) => PyValueError::new_err(m),
    }
}

/// `ids = None` means the wide default: every publicly decodable element.
fn parquet_opts(
    vin: Option<String>,
    year: Option<String>,
    ids: Option<Vec<i32>>,
    batch_size: usize,
    sample_rows: usize,
) -> ParquetOpts {
    ParquetOpts {
        vin,
        year,
        ids: ids.unwrap_or_else(|| ultravin_core::all_public_ids(ultravin_core::Db::embedded())),
        batch_size,
        sample_rows,
    }
}

/// Append one arrow column to `list`, nulls as `None`. The dataset output schema
/// only ever holds these four types (see `parquet_io::build_out_schema`).
fn append_column(list: &Bound<'_, PyList>, arr: &ArrayRef) -> PyResult<()> {
    macro_rules! append_all {
        ($a:expr) => {{
            let a = $a;
            for i in 0..a.len() {
                list.append((!a.is_null(i)).then(|| a.value(i)))?;
            }
        }};
    }
    match arr.data_type() {
        DataType::Utf8 => append_all!(arr.as_string::<i32>()),
        DataType::Int32 => append_all!(arr.as_primitive::<Int32Type>()),
        DataType::Int64 => append_all!(arr.as_primitive::<Int64Type>()),
        DataType::Float64 => append_all!(arr.as_primitive::<Float64Type>()),
        other => {
            return Err(PyValueError::new_err(format!(
                "unsupported output column type {other}"
            )))
        }
    }
    Ok(())
}

/// One chunk as `{column_name: [values]}`.
fn batch_to_dict<'py>(py: Python<'py>, batch: &RecordBatch) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (i, field) in batch.schema().fields().iter().enumerate() {
        let list = PyList::empty(py);
        append_column(&list, batch.column(i))?;
        d.set_item(field.name(), list)?;
    }
    Ok(d)
}

/// Every chunk concatenated into one `{column_name: [values]}`. Every chunk of a
/// source shares one schema — `open_chunks` refuses a directory whose files
/// disagree — so appending by position is safe.
fn batches_to_dict<'py>(
    py: Python<'py>,
    schema: &SchemaRef,
    batches: &[RecordBatch],
) -> PyResult<Bound<'py, PyDict>> {
    let lists: Vec<Bound<'py, PyList>> =
        schema.fields().iter().map(|_| PyList::empty(py)).collect();
    for b in batches {
        for (i, list) in lists.iter().enumerate() {
            append_column(list, b.column(i))?;
        }
    }
    let d = PyDict::new(py);
    for (field, list) in schema.fields().iter().zip(lists) {
        d.set_item(field.name(), list)?;
    }
    Ok(d)
}

/// Decode a parquet file (or directory of them), projecting each row to the
/// requested vPIC element ids.
///
/// With `dst`, the whole job stays in Rust — rows are never Python objects — and
/// the row count comes back. Without it, the decoded columns are collected into
/// a dict, which is O(source) memory and so is for small inputs only.
#[pyfunction]
#[pyo3(signature = (src, dst = None, *, vin = None, year = None, ids = None, batch_size = 65_536, sample_rows = 100))]
// One parameter per documented keyword; collapsing them into a struct would only
// move the argument list into Python.
#[allow(clippy::too_many_arguments)]
fn decode_parquet(
    py: Python<'_>,
    src: PathBuf,
    dst: Option<PathBuf>,
    vin: Option<String>,
    year: Option<String>,
    ids: Option<Vec<i32>>,
    batch_size: usize,
    sample_rows: usize,
) -> PyResult<Py<PyAny>> {
    let opts = parquet_opts(vin, year, ids, batch_size, sample_rows);
    if let Some(dst) = dst {
        let rows = py
            .detach(|| decode_parquet_to_file(&src, &dst, opts))
            .map_err(parquet_err)?;
        return rows.into_py_any(py);
    }
    let (schema, batches) = py
        .detach(|| -> Result<_, ParquetError> {
            let mut iter = open_chunks(&src, opts)?;
            let schema = iter.out_schema.clone();
            let mut batches = Vec::new();
            while let Some(b) = iter.next_chunk()? {
                batches.push(b);
            }
            Ok((schema, batches))
        })
        .map_err(parquet_err)?;
    let schema =
        schema.ok_or_else(|| PyValueError::new_err("source expanded to no parquet files"))?;
    Ok(batches_to_dict(py, &schema, &batches)?.into_any().unbind())
}

/// Streaming form of [`decode_parquet`]: one `{column: [values]}` dict per chunk,
/// so a source larger than memory can be consumed a chunk at a time.
#[pyclass(module = "ultravin._ultravin")]
struct ParquetBatchIter {
    /// A `#[pyclass]` must be `Sync`, and the parquet row-group reader inside is
    /// `Send` but not — the lock is what makes handing the iterator to another
    /// thread legal. Uncontended, and taken once per chunk of `batch_size` rows.
    inner: std::sync::Mutex<ParquetChunkIter>,
}

#[pymethods]
impl ParquetBatchIter {
    #[new]
    #[pyo3(signature = (src, *, vin = None, year = None, ids = None, batch_size = 65_536, sample_rows = 100))]
    fn new(
        py: Python<'_>,
        src: PathBuf,
        vin: Option<String>,
        year: Option<String>,
        ids: Option<Vec<i32>>,
        batch_size: usize,
        sample_rows: usize,
    ) -> PyResult<Self> {
        let opts = parquet_opts(vin, year, ids, batch_size, sample_rows);
        let inner = py.detach(|| open_chunks(&src, opts)).map_err(parquet_err)?;
        Ok(ParquetBatchIter {
            inner: std::sync::Mutex::new(inner),
        })
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Py<PyDict>>> {
        // The lock is taken *inside* `detach`. Taking it first would let a second
        // thread sharing this iterator block on it while holding the GIL, which
        // the holder needs back to return — deadlocking the interpreter.
        let batch = py
            .detach(|| {
                self.inner
                    .lock()
                    .map_err(|_| {
                        ParquetError::Io(
                            "this iterator was left unusable by an earlier panic".to_string(),
                        )
                    })
                    .and_then(|mut it| it.next_chunk())
            })
            .map_err(parquet_err)?;
        match batch {
            Some(b) => Ok(Some(batch_to_dict(py, &b)?.unbind())),
            None => Ok(None),
        }
    }
}

#[pymodule]
fn _ultravin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(decode, m)?)?;
    m.add_function(wrap_pyfunction!(decode_batch, m)?)?;
    m.add_function(wrap_pyfunction!(decode_json, m)?)?;
    m.add_function(wrap_pyfunction!(decode_batch_json, m)?)?;
    m.add_function(wrap_pyfunction!(multi_valued, m)?)?;
    m.add_function(wrap_pyfunction!(elements, m)?)?;
    m.add_function(wrap_pyfunction!(generate, m)?)?;
    m.add_function(wrap_pyfunction!(sweep, m)?)?;
    m.add_function(wrap_pyfunction!(cover_vins, m)?)?;
    m.add_function(wrap_pyfunction!(pairwise, m)?)?;
    m.add_function(wrap_pyfunction!(seeded, m)?)?;
    m.add_function(wrap_pyfunction!(decode_parquet, m)?)?;
    m.add_class::<ParquetBatchIter>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
