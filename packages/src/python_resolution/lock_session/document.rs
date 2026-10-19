//! A bounded replacement document tied to the original lock observation.
//! Formatting a model does not establish its graph or authorize publication.

use super::super::lock_model::LockModelError;
use super::{BoundPythonLock, LockModelLimits, PythonLockModel};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum LockDocumentError {
    #[error("Python lock document limits must be positive")]
    InvalidLimits,
    #[error("Python lock document exceeds its current budget")]
    Limit,
    #[error("Python lock model is invalid")]
    Model,
    #[error("Python lock document encoding is invalid")]
    Encoding,
    #[error("original Python lock observation is no longer current")]
    Revoked,
    #[error("prepared Python lock document belongs to another actual owner")]
    WrongOwner,
}

pub(crate) struct PreparedPythonLockDocument<'lock> {
    owner: &'lock BoundPythonLock,
    model: PythonLockModel,
    bytes: Vec<u8>,
    sha256: [u8; 32],
}
impl PreparedPythonLockDocument<'_> {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub(crate) fn model(&self) -> &PythonLockModel {
        &self.model
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> Result<(), LockDocumentError> {
        if !std::ptr::eq(self.owner, owner) {
            return Err(LockDocumentError::WrongOwner);
        }
        self.owner.recheck().map_err(|_| LockDocumentError::Revoked)
    }
}
impl std::fmt::Debug for PreparedPythonLockDocument<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("PreparedPythonLockDocument")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}
impl BoundPythonLock {
    pub(crate) fn prepare_document(
        &self,
        model: PythonLockModel,
        limits: LockModelLimits,
    ) -> Result<PreparedPythonLockDocument<'_>, LockDocumentError> {
        prepare_with_finish(self, model, limits, || {})
    }
}
fn prepare_with_finish(
    owner: &BoundPythonLock,
    model: PythonLockModel,
    limits: LockModelLimits,
    finish: impl FnOnce(),
) -> Result<PreparedPythonLockDocument<'_>, LockDocumentError> {
    limits.validate().map_err(model_error)?;
    owner.recheck().map_err(|_| LockDocumentError::Revoked)?;
    let original = owner.image.bytes();
    if original.is_some_and(|bytes| bytes.len() > limits.max_document_bytes) {
        return Err(LockDocumentError::Limit);
    }
    if let Some(previous) = owner.model() {
        previous.check_limits(limits).map_err(model_error)?;
    }
    model.check_limits(limits).map_err(model_error)?;
    let mut document = match original {
        None => toml_edit::DocumentMut::new(),
        Some(bytes) => std::str::from_utf8(bytes)
            .map_err(|_| LockDocumentError::Encoding)?
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| LockDocumentError::Encoding)?,
    };
    let encoded = model.encode_projection(limits).map_err(model_error)?;
    let mut projection = encoded
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| LockDocumentError::Encoding)?;
    let mut python = projection
        .remove("python")
        .ok_or(LockDocumentError::Encoding)?;
    // Parsed table positions belong to the projection's separate document.
    // Reusing those offsets would interleave it with existing native tables.
    clear_table_positions(&mut python);
    document.insert("python", python);
    let revision = document
        .get("lockfile_version")
        .and_then(toml_edit::Item::as_integer)
        .unwrap_or(0)
        .max(4);
    match document.get_mut("lockfile_version") {
        Some(toml_edit::Item::Value(value)) => {
            // Keep the native revision's whitespace and inline comment.
            let decor = value.decor().clone();
            *value = toml_edit::Value::from(revision);
            *value.decor_mut() = decor;
        }
        Some(_) => return Err(LockDocumentError::Encoding),
        None => {
            document.insert("lockfile_version", toml_edit::value(revision));
        }
    }
    let mut output = BoundedOutput {
        bytes: Vec::new(),
        maximum: limits.max_document_bytes,
    };
    write!(&mut output, "{document}").map_err(|_| LockDocumentError::Limit)?;
    // Decode through the same bounded root kernel, rather than treating a
    // rendered TOML tree or a document digest as a validated model.
    let checked =
        super::decode_root(&output.bytes, limits).map_err(|_| LockDocumentError::Encoding)?;
    if checked.as_ref() != Some(&model) {
        return Err(LockDocumentError::Encoding);
    }
    let sha256 = Sha256::digest(&output.bytes).into();
    finish();
    owner.recheck().map_err(|_| LockDocumentError::Revoked)?;
    Ok(PreparedPythonLockDocument {
        owner,
        model,
        bytes: output.bytes,
        sha256,
    })
}
fn clear_table_positions(item: &mut toml_edit::Item) {
    fn clear(table: &mut toml_edit::Table) {
        table.set_position(None);
        for (_, child) in table.iter_mut() {
            clear_table_positions(child);
        }
    }
    match item {
        toml_edit::Item::Table(table) => clear(table),
        toml_edit::Item::ArrayOfTables(tables) => {
            for table in tables.iter_mut() {
                clear(table);
            }
        }
        _ => {}
    }
}
fn model_error(error: LockModelError) -> LockDocumentError {
    match error {
        LockModelError::InvalidLimits => LockDocumentError::InvalidLimits,
        LockModelError::Limit(_) => LockDocumentError::Limit,
        _ => LockDocumentError::Model,
    }
}
struct BoundedOutput {
    bytes: Vec<u8>,
    maximum: usize,
}
impl std::fmt::Write for BoundedOutput {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.bytes
            .len()
            .checked_add(value.len())
            .filter(|n| *n <= self.maximum)
            .ok_or(std::fmt::Error)?;
        self.bytes
            .try_reserve(value.len())
            .map_err(|_| std::fmt::Error)?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant},
    };
    fn open(path: &Path) -> BoundPythonLock {
        BoundPythonLock::acquire(
            path,
            Default::default(),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap()
    }
    fn model() -> PythonLockModel {
        PythonLockModel::from_parts(&[], &[], Default::default()).unwrap()
    }
    fn native() -> &'static str {
        "# exact native header\nlockfile_version = 3 # preserve revision comment\n\n[tools]\npython = { version = '3.12', backend = 'core:python' } # native inline\n\n[future]\nwhen = 2026-10-16T00:00:00Z\nvalues = [1, 'two', true]\nliteral = 'PRIVATE_NATIVE_TOKEN'\n"
    }
    #[test]
    fn native_sections_unknown_values_and_comments_survive_python_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pakro.lock");
        fs::write(&path, native()).unwrap();
        let root = open(&path);
        let staged = root.prepare_document(model(), Default::default()).unwrap();
        let output = std::str::from_utf8(staged.bytes()).unwrap();
        assert!(output.starts_with(
            "# exact native header\nlockfile_version = 4 # preserve revision comment\n"
        ));
        assert!(
            output.contains(native().split_once("\n\n").unwrap().1),
            "static fixture output: {output:?}"
        );
        assert!(
            super::super::decode_root(staged.bytes(), Default::default())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            staged.sha256(),
            &<[u8; 32]>::from(Sha256::digest(staged.bytes()))
        );
        assert_eq!(staged.model().graphs().len(), 0);
        assert_eq!(fs::read(&path).unwrap(), native().as_bytes());
        assert!(!format!("{staged:?}").contains("PRIVATE_NATIVE_TOKEN"));
    }
    #[test]
    fn absent_lock_can_be_staged_without_creating_a_lock_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pakro.lock");
        let root = open(&path);
        let staged = root.prepare_document(model(), Default::default()).unwrap();
        staged.recheck_for(&root).unwrap();
        assert!(!path.exists());
        assert!(
            super::super::decode_root(staged.bytes(), Default::default())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "only the original permanent guard"
        );
    }
    #[test]
    fn an_existing_python_projection_is_replaced_while_native_bytes_stay_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pakro.lock");
        let raw = format!(
            "{}\n{}",
            native().replacen("= 3 #", "= 4 #", 1),
            model().encode_projection(Default::default()).unwrap()
        );
        fs::write(&path, &raw).unwrap();
        let root = open(&path);
        let staged = root.prepare_document(model(), Default::default()).unwrap();
        let output = std::str::from_utf8(staged.bytes()).unwrap();
        assert_eq!(output.matches("[python]").count(), 1);
        assert!(
            output.contains(native().split_once("\n\n").unwrap().1),
            "static fixture output: {output:?}"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), raw);
    }
    #[test]
    fn identical_document_data_cannot_rebind_the_actual_owner_or_inode() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let a = one.path().join("pakro.lock");
        let b = two.path().join("pakro.lock");
        fs::write(&a, native()).unwrap();
        fs::write(&b, native()).unwrap();
        let root = open(&a);
        let other = open(&b);
        let staged = root.prepare_document(model(), Default::default()).unwrap();
        assert_eq!(
            staged.recheck_for(&other),
            Err(LockDocumentError::WrongOwner)
        );
        let replacement = one.path().join("replacement");
        fs::write(&replacement, native()).unwrap();
        fs::rename(replacement, &a).unwrap();
        assert_eq!(staged.recheck_for(&root), Err(LockDocumentError::Revoked));
        assert_eq!(fs::read_to_string(&a).unwrap(), native());
    }
    #[test]
    fn external_native_edits_during_formatting_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pakro.lock");
        fs::write(&path, native()).unwrap();
        let root = open(&path);
        let external = b"lockfile_version=4\n# actual external native writer\n";
        assert_eq!(
            prepare_with_finish(&root, model(), Default::default(), || fs::write(
                &path, external
            )
            .unwrap())
            .unwrap_err(),
            LockDocumentError::Revoked
        );
        assert_eq!(fs::read(&path).unwrap(), external);
    }
    #[test]
    fn current_native_wire_and_final_output_have_independent_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pakro.lock");
        fs::write(&path, native()).unwrap();
        let root = open(&path);
        let zero = LockModelLimits {
            max_document_bytes: 0,
            ..Default::default()
        };
        assert_eq!(
            root.prepare_document(model(), zero).unwrap_err(),
            LockDocumentError::InvalidLimits
        );
        let input = LockModelLimits {
            max_document_bytes: native().len() - 1,
            ..Default::default()
        };
        assert_eq!(
            root.prepare_document(model(), input).unwrap_err(),
            LockDocumentError::Limit
        );
        let output = LockModelLimits {
            max_document_bytes: native().len(),
            ..Default::default()
        };
        assert_eq!(
            root.prepare_document(model(), output).unwrap_err(),
            LockDocumentError::Limit
        );
        root.recheck().unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), native());
    }
}
