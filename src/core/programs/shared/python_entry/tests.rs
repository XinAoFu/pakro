use super::*;

#[test]
fn p1_decoded_backend_identity_controls_admission() {
    let python = BackendArg::new("py".into(), Some("core:python".into()));
    let node = BackendArg::new("python-helper".into(), Some("core:node".into()));
    assert!(reject_python_write(PythonWriteOperation::Use, &python).is_err());
    assert!(reject_python_write(PythonWriteOperation::Use, &node).is_ok());
    assert!(reject_python_write(PythonWriteOperation::Force, &python).is_err());
    assert!(reject_python_write(PythonWriteOperation::PostInstall, &python).is_err());
}

#[test]
fn p1_provider_ids_cover_python_adapters_without_scanning_scripts() {
    for provider in ["python", "pip", "uv", "poetry", "//apps/api:uv"] {
        assert!(reject_python_provider_write(PythonWriteOperation::Add, provider).is_err());
    }
    for provider in ["npm", "python-helper", "echo python", "cargo"] {
        assert!(reject_python_provider_write(PythonWriteOperation::Remove, provider).is_ok());
    }
}
