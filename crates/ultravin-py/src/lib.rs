//! PyO3 bindings: exposes `ultravin._ultravin` with `decode`/`decode_batch`.
//! All logic lives in `ultravin-core`; this layer only marshals to Python.

use std::cell::RefCell;

use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};

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
/// VehicleType row id (2 = passenger car, 7 = MPV). Returns fewer than `n` only
/// when the filter matches nothing. Raises `ValueError` when `n` exceeds
/// `GENERATE_MAX`.
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
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
