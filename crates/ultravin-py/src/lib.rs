//! PyO3 bindings: exposes `ultravin._ultravin` with `decode`/`decode_batch`.
//! All logic lives in `ultravin-core`; this layer only marshals to Python.

use pyo3::prelude::*;
use pyo3::types::PyDict;

fn result_to_dict<'py>(py: Python<'py>, vin: &str) -> PyResult<Bound<'py, PyDict>> {
    let r = ultravin_core::decode(vin);
    let d = PyDict::new(py);
    d.set_item("vin", r.vin)?;
    d.set_item("wmi", r.wmi)?;
    d.set_item("descriptor", r.descriptor)?;
    d.set_item("check_digit_valid", r.check_digit_valid)?;
    d.set_item("errors", r.errors)?;
    Ok(d)
}

/// Decode a single VIN to a dict (data-free fields for now).
#[pyfunction]
fn decode<'py>(py: Python<'py>, vin: &str) -> PyResult<Bound<'py, PyDict>> {
    result_to_dict(py, vin)
}

/// Decode a batch of VINs to a list of dicts.
#[pyfunction]
fn decode_batch<'py>(py: Python<'py>, vins: Vec<String>) -> PyResult<Vec<Bound<'py, PyDict>>> {
    vins.iter().map(|v| result_to_dict(py, v)).collect()
}

#[pymodule]
fn _ultravin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(decode, m)?)?;
    m.add_function(wrap_pyfunction!(decode_batch, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
