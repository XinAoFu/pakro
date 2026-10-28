use pyo3::prelude::*;

#[pyfunction]
fn answer() -> i32 {
    let mut buffer = itoa::Buffer::new();
    buffer.format(42).parse().unwrap()
}

#[pymodule]
fn p6_rust_demo(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(answer, module)?)?;
    Ok(())
}
