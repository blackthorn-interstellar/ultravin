//! PyO3 bindings: exposes `ultravin._ultravin` with `decode`/`decode_batch`.
//! All logic lives in `ultravin-core`; this layer only marshals to Python.

use std::cell::RefCell;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};

use ultravin_core::{DecodeResult, DecodedElement};

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

/// Decode a single VIN to a dict.
#[pyfunction]
fn decode<'py>(py: Python<'py>, vin: &str) -> PyResult<Bound<'py, PyDict>> {
    result_to_dict(py, &ultravin_core::decode(vin))
}

/// Decode a batch of VINs to a list of dicts.
///
/// The decode work runs in parallel with the GIL released; only the final
/// marshalling of results into Python dicts holds the GIL.
#[pyfunction]
fn decode_batch<'py>(py: Python<'py>, vins: Vec<String>) -> PyResult<Vec<Bound<'py, PyDict>>> {
    let results = py.allow_threads(|| ultravin_core::decode_batch(&vins));
    results.iter().map(|r| result_to_dict(py, r)).collect()
}

/// Decode a single VIN to a JSON object string (same shape as `decode`).
#[pyfunction]
fn decode_json(vin: &str) -> String {
    ultravin_core::decode_json(vin)
}

/// Decode a batch of VINs to a single JSON array string.
///
/// Decode *and* JSON serialization run in parallel with the GIL released; the
/// caller gets back one string (`json.loads` it for a list equal to
/// `decode_batch`). For large batches this is several times faster than
/// `decode_batch`, which must build a ~15-key dict per element under the GIL.
#[pyfunction]
fn decode_batch_json(py: Python<'_>, vins: Vec<String>) -> String {
    py.allow_threads(|| ultravin_core::decode_batch_json(&vins))
}

#[pymodule]
fn _ultravin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(decode, m)?)?;
    m.add_function(wrap_pyfunction!(decode_batch, m)?)?;
    m.add_function(wrap_pyfunction!(decode_json, m)?)?;
    m.add_function(wrap_pyfunction!(decode_batch_json, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
