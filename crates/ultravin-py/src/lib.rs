//! PyO3 bindings: exposes `ultravin._ultravin` with `decode`/`decode_batch`.
//! All logic lives in `ultravin-core`; this layer only marshals to Python.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use ultravin_core::{DecodeResult, DecodedElement};

fn elem_to_dict<'py>(py: Python<'py>, e: &DecodedElement) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("group_name", &e.group_name)?;
    d.set_item("variable", &e.variable)?;
    d.set_item("value", &e.value)?;
    d.set_item("element_id", e.element_id)?;
    d.set_item("attribute_id", &e.attribute_id)?;
    d.set_item("code", &e.code)?;
    d.set_item("data_type", &e.data_type)?;
    d.set_item("decode", &e.decode)?;
    d.set_item("source", &e.source)?;
    d.set_item("pattern_id", e.pattern_id)?;
    d.set_item("vin_schema_id", e.vin_schema_id)?;
    d.set_item("keys", &e.keys)?;
    d.set_item("created_on", e.created_on)?;
    d.set_item("wmi_id", e.wmi_id)?;
    d.set_item("to_be_qced", e.to_be_qced)?;
    Ok(d)
}

fn result_to_dict<'py>(py: Python<'py>, r: &DecodeResult) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("vin", &r.vin)?;
    d.set_item("wmi", &r.wmi)?;
    d.set_item("descriptor", &r.descriptor)?;
    d.set_item("model_year", r.model_year)?;
    d.set_item("error_codes", r.error_codes.clone())?;
    d.set_item("check_digit_valid", r.check_digit_valid)?;
    d.set_item("corrected_vin", &r.corrected_vin)?;
    let elems = PyList::empty(py);
    for e in &r.elements {
        elems.append(elem_to_dict(py, e)?)?;
    }
    d.set_item("elements", elems)?;
    Ok(d)
}

/// Decode a single VIN to a dict.
#[pyfunction]
fn decode<'py>(py: Python<'py>, vin: &str) -> PyResult<Bound<'py, PyDict>> {
    result_to_dict(py, &ultravin_core::decode(vin))
}

/// Decode a batch of VINs to a list of dicts.
#[pyfunction]
fn decode_batch<'py>(py: Python<'py>, vins: Vec<String>) -> PyResult<Vec<Bound<'py, PyDict>>> {
    vins.iter()
        .map(|v| result_to_dict(py, &ultravin_core::decode(v)))
        .collect()
}

#[pymodule]
fn _ultravin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(decode, m)?)?;
    m.add_function(wrap_pyfunction!(decode_batch, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
