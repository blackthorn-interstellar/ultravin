//! PyO3 bindings: exposes `ultravin._ultravin` with `decode`/`decode_batch`.
//! All logic lives in `ultravin`; this layer only marshals to Python.

use std::any::Any;
use std::cell::RefCell;
use std::ffi::CStr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arrow_array::array::make_array;
use arrow_array::ffi::{from_ffi, FFI_ArrowArray};
use arrow_array::ffi_stream::{ArrowArrayStreamReader, FFI_ArrowArrayStream};
use arrow_array::{Array, RecordBatch, RecordBatchIterator, RecordBatchReader, StructArray};
use arrow_schema::ffi::FFI_ArrowSchema;
use arrow_schema::{ArrowError as ArrowRsError, Field, Schema, SchemaRef};
use pyo3::exceptions::{PyImportError, PyOSError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyCapsule, PyDateTime, PyDict, PyList, PyString};

use ultravin::parquet_io::{
    check_dst_outside_src, open_chunks, write_parquet, ParquetChunkIter, ParquetOpts,
};
use ultravin::{
    ArrowDecoder, ArrowError, ArrowOpts, ColumnNames, ColumnSpec, DecodeResult, DecodedElement,
    FlatResult, FlatValue,
};

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
    /// re-keys on pid (see `ultravin::lib`). If this module ever opts into
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

/// The default shape: header fields, then one `attributes` dict of
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
///
/// The default is the attributes shape; `full=True` swaps in the per-element
/// provenance list, which costs ~615 dict stores per VIN instead of ~41.
#[pyfunction]
#[pyo3(signature = (vin, *, year = None, full = false))]
fn decode<'py>(
    py: Python<'py>,
    vin: &str,
    year: Option<i32>,
    full: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let r = ultravin::decode(vin, year);
    if full {
        result_to_dict(py, &r)
    } else {
        flat_to_dict(py, &r.into())
    }
}

/// Decode a batch of VINs to a list of dicts.
///
/// The decode work runs in parallel with the GIL released; only the final
/// marshalling of results into Python dicts holds the GIL.
#[pyfunction]
#[pyo3(signature = (vins, *, years = None, full = false))]
fn decode_batch<'py>(
    py: Python<'py>,
    vins: Vec<String>,
    years: Option<Vec<Option<i32>>>,
    full: bool,
) -> PyResult<Vec<Bound<'py, PyDict>>> {
    check_years(&vins, &years)?;
    let years = years.as_deref();
    if full {
        let results = py.detach(|| ultravin::decode_batch(&vins, years));
        return results.iter().map(|r| result_to_dict(py, r)).collect();
    }
    // Flattening happens inside the parallel region, so the GIL-held part is only
    // the (much smaller) dict build.
    let results = py.detach(|| ultravin::decode_batch_flat(&vins, years));
    results.iter().map(|r| flat_to_dict(py, r)).collect()
}

/// Decode a single VIN to a JSON object string (same shape as `decode`).
#[pyfunction]
#[pyo3(signature = (vin, *, year = None, full = false))]
fn decode_json(vin: &str, year: Option<i32>, full: bool) -> String {
    if full {
        ultravin::decode_json(vin, year)
    } else {
        ultravin::decode_json_flat(vin, year)
    }
}

/// The variable names whose `attributes` value is always a list.
#[pyfunction]
fn multi_valued() -> Vec<&'static str> {
    ultravin::multi_valued_variables(ultravin::Db::embedded())
}

/// The static element table: one `(variable, element_id, group_name, code,
/// data_type, decode)` dict per public element, for callers that want to pin to
/// element ids rather than the vPIC variable names (which NHTSA renames between
/// data releases).
#[pyfunction]
fn elements(py: Python<'_>) -> PyResult<Vec<Bound<'_, PyDict>>> {
    let db = ultravin::Db::embedded();
    let mut out = Vec::new();
    for e in db.elements() {
        let Some(decode) = ultravin::public_decode(db, e) else {
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
#[pyo3(signature = (vins, *, years = None, full = false))]
fn decode_batch_json(
    py: Python<'_>,
    vins: Vec<String>,
    years: Option<Vec<Option<i32>>>,
    full: bool,
) -> PyResult<String> {
    check_years(&vins, &years)?;
    Ok(py.detach(|| {
        let years = years.as_deref();
        if full {
            ultravin::decode_batch_json(&vins, years)
        } else {
            ultravin::decode_batch_json_flat(&vins, years)
        }
    }))
}

/// Upper bound on a single `generate` request. A larger `n` is almost certainly a
/// mistake, and left unchecked it drives a multi-terabyte allocation that aborts
/// the process (uncatchable) rather than raising. Ten million VINs is already far
/// past any legitimate fixture.
const GENERATE_MAX: usize = 10_000_000;

/// A caller-supplied `now` as the `(now_micros, current_year)` pair the core
/// `generate` takes, both derived from that one instant so they cannot disagree.
///
/// A naive datetime is read as UTC, not as local time: the point of pinning a
/// clock is that the fixture replays identically, and local time would make the
/// same literal mean a different instant in another timezone. An aware one is
/// converted, so both spellings of an instant give the same pair.
///
/// Truncated to whole seconds because that is all `now_micros` carries; the WMI
/// publication dates it ends up compared against are calendar days anyway.
fn clock_from(now: &Bound<'_, PyDateTime>) -> PyResult<(i64, i32)> {
    let py = now.py();
    // `timestamp()` is already UTC epoch seconds for an aware datetime, whatever
    // zone it is spelled in, so only a naive one needs to be told what it meant.
    let secs: f64 = if now.getattr(intern!(py, "tzinfo"))?.is_none() {
        let utc = py
            .import(intern!(py, "datetime"))?
            .getattr(intern!(py, "timezone"))?
            .getattr(intern!(py, "utc"))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "tzinfo"), &utc)?;
        now.call_method(intern!(py, "replace"), (), Some(&kwargs))?
            .call_method0(intern!(py, "timestamp"))?
            .extract()?
    } else {
        now.call_method0(intern!(py, "timestamp"))?.extract()?
    };
    let micros = (secs.floor() as i64).saturating_mul(1_000_000);
    Ok((micros, ultravin::current_year_at(micros)))
}

/// Generate `n` valid VINs, deterministic for a given `seed`.
///
/// Each VIN comes from a real WMI, a schema that WMI uses, and one of that
/// schema's patterns, so it decodes to real attributes rather than to an
/// unknown-manufacturer error. Filters are conjunctive; `vehicle_type` is a
/// VehicleType row id (2 = passenger car, 7 = MPV) and `year` is the year the
/// VIN decodes to, not merely the character in position 10. Returns fewer than
/// `n` when the filter matches nothing — including a `wmi` that is in the data
/// but not published yet, which the decoder refuses to resolve and this refuses
/// to emit. The result may repeat a VIN, increasingly so as `n` grows; `seeded`
/// is the deduplicated corpus. Raises `ValueError` when `n` exceeds
/// `GENERATE_MAX`.
///
/// `now` freezes the clock the core function otherwise reads here, which is what
/// makes a seeded fixture reproducible past a year rollover. A `now` before the
/// Unix epoch leaves every WMI's publication date in the future, so nothing is
/// drawable and the result is empty rather than an error.
#[pyfunction]
#[pyo3(signature = (n, *, seed = 0, wmi = None, make = None, year = None, vehicle_type = None, now = None))]
// One parameter per documented keyword; collapsing them into a struct would only
// move the argument list into Python.
#[allow(clippy::too_many_arguments)]
fn generate<'py>(
    py: Python<'py>,
    n: usize,
    seed: u64,
    wmi: Option<String>,
    make: Option<String>,
    year: Option<i32>,
    vehicle_type: Option<i32>,
    now: Option<Bound<'py, PyDateTime>>,
) -> PyResult<Vec<String>> {
    if n > GENERATE_MAX {
        return Err(PyValueError::new_err(format!(
            "n={n} is too large; generate at most {GENERATE_MAX} VINs per call"
        )));
    }
    let (now_micros, current_year) = match &now {
        Some(dt) => clock_from(dt)?,
        None => {
            // One reading, derived twice. Calling `now_micros` and `current_year`
            // separately reads the clock twice, and the two can straddle a second
            // — or, once a year, the model-year boundary itself.
            let micros = ultravin::now_micros();
            (micros, ultravin::current_year_at(micros))
        }
    };
    let filter = ultravin::Filter {
        wmi,
        make,
        year,
        vehicle_type,
    };
    Ok(py.detach(|| {
        ultravin::generate(
            ultravin::Db::embedded(),
            n,
            seed,
            &filter,
            now_micros,
            current_year,
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
        None => ultravin::Dimension::ALL.to_vec(),
        Some(names) => names
            .iter()
            .map(|n| match n.as_str() {
                "wmi" => Ok(ultravin::Dimension::Wmi),
                "pattern" => Ok(ultravin::Dimension::Pattern),
                "engine" => Ok(ultravin::Dimension::Engine),
                "vspec" => Ok(ultravin::Dimension::VehicleSpec),
                "exception" => Ok(ultravin::Dimension::Exception),
                "default" => Ok(ultravin::Dimension::Default),
                other => Err(PyValueError::new_err(format!("unknown dimension: {other}"))),
            })
            .collect::<PyResult<Vec<_>>>()?,
    };
    Ok(py.detach(|| ultravin::sweep(ultravin::Db::embedded(), &dims, ultravin::current_year())))
}

/// The smallest VIN set that exercises every decode behaviour this data month
/// can reach — computed when the artifact was built, so it costs nothing here.
///
/// Use it as a decoder test corpus: a few hundred VINs that between them touch
/// every resolution rung, error code, conversion and tiebreak the data supports.
#[pyfunction]
fn cover_vins() -> Vec<String> {
    ultravin::Db::embedded().cover()
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
    py.detach(|| ultravin::pairwise(ultravin::Db::embedded(), ultravin::current_year(), limit))
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
    py.detach(|| ultravin::seeded(ultravin::Db::embedded(), ultravin::current_year(), limit))
}

/// `Config` is a caller mistake (unknown column, bad element id) and `Io` is the
/// filesystem talking, so they get the two exceptions Python callers already
/// catch for those things.
fn arrow_err(e: ArrowError) -> PyErr {
    match e {
        ArrowError::Io(m) => PyOSError::new_err(m),
        ArrowError::Config(m) => PyValueError::new_err(m),
    }
}

/// The two Arrow C data interface capsule names, spelled as the protocol does.
const STREAM_CAPSULE: &CStr = c"arrow_array_stream";
const SCHEMA_CAPSULE: &CStr = c"arrow_schema";
const ARRAY_CAPSULE: &CStr = c"arrow_array";

/// The column an unnamed one-array Arrow source is understood as. A bare string
/// array carries no field name, and a VIN column is the only thing this decoder
/// could be being handed.
const BARE_ARRAY_COLUMN: &str = "vin";

/// `columns = None` means the wide default: every publicly decodable element.
///
/// An entry is an `element_id` (`int`) or a variable name (`str`). Ids are the
/// key NHTSA does not rename between data releases; names are the convenience.
fn column_specs(columns: Option<Vec<Bound<'_, PyAny>>>) -> PyResult<Vec<ColumnSpec>> {
    let Some(items) = columns else {
        return Ok(ultravin::all_public_ids(ultravin::Db::embedded())
            .into_iter()
            .map(ColumnSpec::Id)
            .collect());
    };
    let mut specs = Vec::with_capacity(items.len());
    for item in items {
        if let Ok(name) = item.cast::<PyString>() {
            specs.push(ColumnSpec::Name(name.to_cow()?.into_owned()));
            continue;
        }
        // `bool` is an `int` subclass in Python, so `True` would extract as
        // element id 1 — a real element, silently projected.
        match item.extract::<i32>() {
            Ok(id) if !item.is_instance_of::<PyBool>() => specs.push(ColumnSpec::Id(id)),
            _ => {
                return Err(PyTypeError::new_err(format!(
                    "columns entries are element ids (int) or variable names (str); got {}",
                    type_name(&item)
                )))
            }
        }
    }
    Ok(specs)
}

/// How to label the projected columns.
///
/// A schema-drift decision: `"variable"` reads better, `"id"` (`attr_<id>`)
/// survives NHTSA renaming a variable between monthly data releases.
fn column_names_from(mode: &str) -> PyResult<ColumnNames> {
    match mode {
        "variable" => Ok(ColumnNames::Variable),
        "id" => Ok(ColumnNames::Id),
        other => Err(PyValueError::new_err(format!(
            "column_names must be \"variable\" or \"id\"; got {other:?}"
        ))),
    }
}

/// A Python object's type name, for an error message. Falls back rather than
/// failing: a broken `__name__` must not replace the real complaint.
fn type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type()
        .name()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "an unknown type".to_string())
}

/// Move the `FFI_ArrowArrayStream` out of a producer's capsule.
///
/// The C data interface transfers ownership on export, so the struct is moved
/// out and an empty one left in its place — the capsule's own destructor then
/// finds nothing to release and cannot double-free the stream we now own.
fn stream_from_capsule(capsule: &Bound<'_, PyCapsule>) -> PyResult<FFI_ArrowArrayStream> {
    let ptr = capsule.pointer_checked(Some(STREAM_CAPSULE))?.as_ptr() as *mut FFI_ArrowArrayStream;
    // SAFETY: the capsule is named `arrow_array_stream`, which by protocol means
    // it holds exactly one initialized `FFI_ArrowArrayStream` for us to take.
    Ok(unsafe { std::ptr::replace(ptr, FFI_ArrowArrayStream::empty()) })
}

/// One `RecordBatch` imported from an `__arrow_c_array__` pair of capsules.
///
/// A struct array is already a batch of named columns. Anything else is a single
/// unnamed column, which can only be the VINs — give it the name the decoder
/// autodetects and pass it through.
fn batch_from_capsules(
    schema_cap: &Bound<'_, PyCapsule>,
    array_cap: &Bound<'_, PyCapsule>,
) -> PyResult<RecordBatch> {
    let schema_ptr =
        schema_cap.pointer_checked(Some(SCHEMA_CAPSULE))?.as_ptr() as *mut FFI_ArrowSchema;
    let array_ptr = array_cap.pointer_checked(Some(ARRAY_CAPSULE))?.as_ptr() as *mut FFI_ArrowArray;
    // SAFETY: both capsules are protocol-named, so each holds one initialized
    // struct; both are moved out so the capsules' destructors release nothing.
    let (schema, array) = unsafe {
        (
            std::ptr::replace(schema_ptr, FFI_ArrowSchema::empty()),
            std::ptr::replace(array_ptr, FFI_ArrowArray::empty()),
        )
    };
    // SAFETY: `array` and `schema` were produced together by one exporter, which
    // is exactly the pairing `from_ffi` requires.
    let data = unsafe { from_ffi(array, &schema) }
        .map_err(|e| PyValueError::new_err(format!("importing an Arrow array: {e}")))?;
    let array = make_array(data);
    if let Some(st) = array.as_any().downcast_ref::<StructArray>() {
        return Ok(RecordBatch::from(st));
    }
    let field = Field::new(BARE_ARRAY_COLUMN, array.data_type().clone(), true);
    RecordBatch::try_new(Arc::new(Schema::new(vec![field])), vec![array])
        .map_err(|e| PyValueError::new_err(format!("importing an Arrow array: {e}")))
}

/// The Arrow input a source exposes, as a reader the decoder can pull from.
/// A stream is taken as-is; a single array becomes a one-batch stream.
fn arrow_input(source: &Bound<'_, PyAny>) -> PyResult<Box<dyn RecordBatchReader + Send>> {
    if source.hasattr(intern!(source.py(), "__arrow_c_stream__"))? {
        let capsule = source.call_method0(intern!(source.py(), "__arrow_c_stream__"))?;
        let stream = stream_from_capsule(capsule.cast::<PyCapsule>()?)?;
        let reader = ArrowArrayStreamReader::try_new(stream)
            .map_err(|e| PyValueError::new_err(format!("importing an Arrow stream: {e}")))?;
        return Ok(Box::new(reader));
    }
    let capsules = source.call_method0(intern!(source.py(), "__arrow_c_array__"))?;
    let (schema_cap, array_cap): (Bound<'_, PyAny>, Bound<'_, PyAny>) = capsules.extract()?;
    let batch = batch_from_capsules(
        schema_cap.cast::<PyCapsule>()?,
        array_cap.cast::<PyCapsule>()?,
    )?;
    let schema = batch.schema();
    Ok(Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema)))
}

/// Run one reader step, turning a panic into a stream error.
///
/// The exported stream's `get_next` is an `extern "C"` function that arrow-rs
/// does not wrap in `catch_unwind`, so a panic anywhere under it — a rayon pool
/// the process cannot grow under a pids limit, an i32 offset overflow assembling
/// an oversized string column — would unwind across the C boundary and abort the
/// interpreter instead of raising. Every batch on the capsule path passes
/// through here, so the worst case is a failed stream rather than a dead process.
///
/// `AssertUnwindSafe` is the honest label: a panic can leave the reader
/// half-advanced, which is why this reports an error rather than resuming.
fn caught<F>(step: F) -> Option<Result<RecordBatch, ArrowRsError>>
where
    F: FnOnce() -> Option<Result<RecordBatch, ArrowRsError>>,
{
    match catch_unwind(AssertUnwindSafe(step)) {
        Ok(item) => item,
        Err(payload) => Some(Err(ArrowRsError::ComputeError(format!(
            "panic while decoding: {}",
            panic_message(payload.as_ref())
        )))),
    }
}

/// The message out of a panic payload, which is a `&str` or a `String` for every
/// panic the standard macros raise.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "no message".to_string()
    }
}

/// A dataset source as a `RecordBatchReader`.
///
/// [`ParquetChunkIter`] already yields decoded batches; this only restates its
/// schema and its errors in the shapes the Arrow C data interface takes, so a
/// parquet source and an Arrow source reach the same exit.
struct ParquetReader {
    inner: ParquetChunkIter,
    schema: SchemaRef,
}

impl Iterator for ParquetReader {
    type Item = Result<RecordBatch, ArrowRsError>;

    fn next(&mut self) -> Option<Self::Item> {
        let inner = &mut self.inner;
        caught(move || inner.next().map(|r| r.map_err(Into::into)))
    }
}

impl RecordBatchReader for ParquetReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
}

/// Decode every batch an input reader yields, in the projected output shape.
///
/// This is what an exported decode stream runs: no Python object is touched in
/// `next`, so once it is inside an `FFI_ArrowArrayStream` the consumer drives the
/// whole decode from C.
struct DecodingReader {
    input: Box<dyn RecordBatchReader + Send>,
    decoder: ArrowDecoder,
}

impl Iterator for DecodingReader {
    type Item = Result<RecordBatch, ArrowRsError>;

    fn next(&mut self) -> Option<Self::Item> {
        let DecodingReader { input, decoder } = self;
        caught(move || match input.next()? {
            Ok(batch) => Some(decoder.decode_batch(&batch).map_err(Into::into)),
            Err(e) => Some(Err(e)),
        })
    }
}

impl RecordBatchReader for DecodingReader {
    fn schema(&self) -> SchemaRef {
        self.decoder.out_schema().clone()
    }
}

/// A one-shot stream of decoded Arrow batches.
///
/// Both output doors — the C stream capsule and `to_parquet` — consume the
/// source, so the reader is taken out on first use and a second attempt raises
/// rather than handing back a silently truncated result.
#[pyclass(module = "ultravin._ultravin")]
struct DecodeStream {
    /// `None` once consumed. A `#[pyclass]` must be `Sync` and a reader is only
    /// `Send`, so the lock is also what makes sharing one across threads legal.
    reader: Mutex<Option<Box<dyn RecordBatchReader + Send>>>,
    schema: SchemaRef,
    /// The parquet source, when there was one — `to_parquet` must not write over
    /// the file it is still reading.
    src: Option<PathBuf>,
    row_group: usize,
}

impl DecodeStream {
    /// Take the reader, or say why there isn't one.
    fn take(&self) -> PyResult<Box<dyn RecordBatchReader + Send>> {
        self.reader
            .lock()
            .map_err(|_| {
                PyRuntimeError::new_err("this stream was left unusable by an earlier panic")
            })?
            .take()
            .ok_or_else(|| {
                PyRuntimeError::new_err(
                    "this DecodeStream has already been consumed; call decode_stream() again \
                     to re-read the source",
                )
            })
    }
}

#[pymethods]
impl DecodeStream {
    /// The Arrow C stream export: hand the decode to pyarrow, polars, duckdb, …
    ///
    /// `requested_schema` is accepted and ignored, which the protocol allows —
    /// the output schema is fixed by the projection and cannot be renegotiated.
    #[pyo3(signature = (requested_schema = None))]
    fn __arrow_c_stream__<'py>(
        &self,
        py: Python<'py>,
        requested_schema: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyCapsule>> {
        let _ = requested_schema;
        PyCapsule::new_with_value(py, FFI_ArrowArrayStream::new(self.take()?), STREAM_CAPSULE)
    }

    /// The output schema, known before a single row is decoded.
    fn __arrow_c_schema__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyCapsule>> {
        let schema = FFI_ArrowSchema::try_from(self.schema.as_ref())
            .map_err(|e| PyValueError::new_err(format!("exporting the output schema: {e}")))?;
        PyCapsule::new_with_value(py, schema, SCHEMA_CAPSULE)
    }

    /// Decode straight to a parquet file, returning the rows written.
    ///
    /// The whole job stays in Rust with the GIL released — no row is ever a
    /// Python object — and peak memory is one chunk.
    fn to_parquet(&self, py: Python<'_>, dst: PathBuf) -> PyResult<usize> {
        // Refused before the reader is taken, so a rejected destination leaves
        // the stream usable — and, more to the point, leaves the source intact.
        if let Some(src) = &self.src {
            check_dst_outside_src(src, &dst).map_err(arrow_err)?;
        }
        let reader = self.take()?;
        let schema = self.schema.clone();
        py.detach(|| {
            let batches = reader.map(|r| r.map_err(ArrowError::from));
            write_parquet(batches, schema, &dst, self.row_group)
        })
        .map_err(arrow_err)
    }

    /// The decode as a pandas `DataFrame`, via pyarrow.
    ///
    /// pyarrow is imported lazily and is not an ultravin dependency: everything
    /// else here reads and writes Arrow without it.
    fn to_pandas<'py>(slf: Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let pa = py.import(intern!(py, "pyarrow")).map_err(|e| {
            if e.is_instance_of::<PyImportError>(py) {
                PyImportError::new_err("to_pandas() requires pyarrow: pip install pyarrow")
            } else {
                e
            }
        })?;
        pa.call_method1(intern!(py, "table"), (slf,))?
            .call_method0(intern!(py, "to_pandas"))
    }
}

/// Decode a dataset into a stream of Arrow batches.
///
/// `source` is a parquet file, a directory of them, or any object exposing the
/// Arrow C data interface (`__arrow_c_stream__` or `__arrow_c_array__`) — a
/// pyarrow `Table`/`RecordBatchReader`, a polars `DataFrame`, a duckdb result.
///
/// `column_names` labels the projection: `"variable"` (the default) uses the vPIC
/// variable name, `"id"` uses `attr_<element_id>`, which does not move when NHTSA
/// renames a variable. Either way both keys ride along as field metadata.
#[pyfunction]
#[pyo3(signature = (source, *, vin_column = None, year_column = None, columns = None, column_names = "variable", batch_size = 65_536, sample_rows = 100))]
// One parameter per documented keyword; collapsing them into a struct would only
// move the argument list into Python.
#[allow(clippy::too_many_arguments)]
fn decode_stream(
    py: Python<'_>,
    source: &Bound<'_, PyAny>,
    vin_column: Option<String>,
    year_column: Option<String>,
    columns: Option<Vec<Bound<'_, PyAny>>>,
    column_names: &str,
    batch_size: usize,
    sample_rows: usize,
) -> PyResult<DecodeStream> {
    // A list of VINs is the one wrong argument worth naming outright: it is what
    // a reader of `decode_batch` would try first, and it is not a dataset. It is
    // checked before the projection is validated, so passing a list *and* a bad
    // column says which one to fix first.
    if source.is_instance_of::<PyList>() {
        return Err(PyTypeError::new_err(
            "decode_stream() takes a dataset, not a list of VINs — use decode_batch(vins) for that",
        ));
    }
    let specs = column_specs(columns)?;
    let names = column_names_from(column_names)?;
    if source.hasattr(intern!(py, "__arrow_c_stream__"))?
        || source.hasattr(intern!(py, "__arrow_c_array__"))?
    {
        let input = arrow_input(source)?;
        let opts = ArrowOpts {
            vin: vin_column,
            year: year_column,
            columns: specs,
            names,
        };
        let decoder = ArrowDecoder::new(&input.schema(), &opts).map_err(arrow_err)?;
        let schema = decoder.out_schema().clone();
        return Ok(DecodeStream {
            reader: Mutex::new(Some(Box::new(DecodingReader { input, decoder }))),
            schema,
            src: None,
            row_group: batch_size,
        });
    }
    let src: PathBuf = source.extract().map_err(|_| {
        PyTypeError::new_err(format!(
            "decode_stream() takes a parquet file or directory path (str | os.PathLike), \
             or an object implementing the Arrow C data interface \
             (__arrow_c_stream__ / __arrow_c_array__); got {}",
            type_name(source)
        ))
    })?;
    let opts = ParquetOpts {
        vin: vin_column,
        year: year_column,
        columns: specs,
        names,
        batch_size,
        sample_rows,
    };
    let iter = py.detach(|| open_chunks(&src, opts)).map_err(arrow_err)?;
    let schema = iter
        .out_schema
        .clone()
        .ok_or_else(|| PyValueError::new_err("source expanded to no parquet files"))?;
    Ok(DecodeStream {
        reader: Mutex::new(Some(Box::new(ParquetReader {
            inner: iter,
            schema: schema.clone(),
        }))),
        schema,
        src: Some(src),
        row_group: batch_size,
    })
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
    m.add_function(wrap_pyfunction!(decode_stream, m)?)?;
    m.add_class::<DecodeStream>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
