//! PyO3 bindings: exposes `ultravin._ultravin` with `decode`/`decode_batch`.
//! All logic lives in `ultravin-core`; this layer only marshals to Python.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use ultravin_core::{DecodeResult, DecodedElement};

fn elem_to_dict<'py>(py: Python<'py>, e: &DecodedElement) -> PyResult<Bound<'py, PyDict>> {
    // `intern!` reuses one cached `PyString` per key per interpreter instead of
    // allocating a fresh key string on every `set_item` — these 15 keys recur for
    // every element of every decode, so this is the bulk of the marshalling cost.
    let d = PyDict::new(py);
    d.set_item(intern!(py, "group_name"), &e.group_name)?;
    d.set_item(intern!(py, "variable"), &e.variable)?;
    d.set_item(intern!(py, "value"), &e.value)?;
    d.set_item(intern!(py, "element_id"), e.element_id)?;
    d.set_item(intern!(py, "attribute_id"), &e.attribute_id)?;
    d.set_item(intern!(py, "code"), &e.code)?;
    d.set_item(intern!(py, "data_type"), &e.data_type)?;
    d.set_item(intern!(py, "decode"), &e.decode)?;
    d.set_item(intern!(py, "source"), &e.source)?;
    d.set_item(intern!(py, "pattern_id"), e.pattern_id)?;
    d.set_item(intern!(py, "vin_schema_id"), e.vin_schema_id)?;
    d.set_item(intern!(py, "keys"), &e.keys)?;
    d.set_item(intern!(py, "created_on"), e.created_on)?;
    d.set_item(intern!(py, "wmi_id"), e.wmi_id)?;
    d.set_item(intern!(py, "to_be_qced"), e.to_be_qced)?;
    Ok(d)
}

fn result_to_dict<'py>(py: Python<'py>, r: &DecodeResult) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item(intern!(py, "vin"), &r.vin)?;
    d.set_item(intern!(py, "wmi"), &r.wmi)?;
    d.set_item(intern!(py, "descriptor"), &r.descriptor)?;
    d.set_item(intern!(py, "model_year"), r.model_year)?;
    d.set_item(intern!(py, "error_codes"), &r.error_codes)?;
    d.set_item(intern!(py, "check_digit_valid"), r.check_digit_valid)?;
    d.set_item(intern!(py, "corrected_vin"), &r.corrected_vin)?;
    let elems = PyList::empty(py);
    for e in &r.elements {
        elems.append(elem_to_dict(py, e)?)?;
    }
    d.set_item(intern!(py, "elements"), elems)?;
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

#[pymodule]
fn _ultravin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(decode, m)?)?;
    m.add_function(wrap_pyfunction!(decode_batch, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
