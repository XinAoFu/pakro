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
    let original = owner.document_bytes();
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
    if let Some(previous) = document.get("python") {
        retain_projection_format(previous, &mut python);
    }
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

/// The original image was decoded by the same bounded strict model parser.
/// Preserve spelling and comments only while replacing the exact model data;
/// absent records and fields are removed rather than retained as stale facts.
fn retain_projection_format(previous: &toml_edit::Item, next: &mut toml_edit::Item) {
    let same_shape = matches!(
        (previous, &*next),
        (toml_edit::Item::Table(_), toml_edit::Item::Table(_))
            | (
                toml_edit::Item::ArrayOfTables(_),
                toml_edit::Item::ArrayOfTables(_)
            )
            | (toml_edit::Item::Value(_), toml_edit::Item::Value(_))
    );
    if !same_shape {
        // The strict decoder also accepts inline representations of its map
        // and sequence fields. Keep those unchanged when their data is equal.
        if let (Ok(previous_value), Ok(next_value)) =
            (previous.clone().into_value(), next.clone().into_value())
        {
            if same_value(&previous_value, &next_value) {
                *next = previous.clone();
                return;
            }
        }
        if let toml_edit::Item::Table(next) = next {
            if let Ok(previous) = previous.clone().into_table() {
                retain_table_format(&previous, next);
            }
            return;
        }
        if let toml_edit::Item::ArrayOfTables(next) = next {
            if let Ok(previous) = previous.clone().into_array_of_tables() {
                retain_records_format(&previous, next);
            }
            return;
        }
    }
    match (previous, next) {
        (toml_edit::Item::Table(previous), toml_edit::Item::Table(next)) => {
            retain_table_format(previous, next);
        }
        (toml_edit::Item::ArrayOfTables(previous), toml_edit::Item::ArrayOfTables(next)) => {
            retain_records_format(previous, next);
        }
        (toml_edit::Item::Value(previous), toml_edit::Item::Value(next)) => {
            retain_value_format(previous, next);
        }
        _ => {}
    }
}

fn retain_records_format(previous: &toml_edit::ArrayOfTables, next: &mut toml_edit::ArrayOfTables) {
    let old: std::collections::BTreeMap<_, _> = previous
        .iter()
        .filter_map(|table| record_identity(table).map(|key| (key, table)))
        .collect();
    for table in next.iter_mut() {
        if let Some(previous) = record_identity(table).and_then(|key| old.get(&key)) {
            retain_table_format(previous, table);
        }
    }
}

fn retain_table_format(previous: &toml_edit::Table, next: &mut toml_edit::Table) {
    let mut merged = previous.clone();
    let removed: Vec<_> = previous
        .iter()
        .filter(|(key, _)| !next.contains_key(key))
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in removed {
        merged.remove(&key);
    }
    for (key, item) in next.iter() {
        let mut replacement = item.clone();
        if let Some(previous) = merged.get_mut(key) {
            retain_projection_format(previous, &mut replacement);
            *previous = replacement;
        } else if let Some(key) = next.key(key) {
            merged.insert_formatted(key, replacement);
        }
    }
    *next = merged;
}

fn record_identity(table: &toml_edit::Table) -> Option<Vec<String>> {
    // Every sequence in the strict Python projection has one of these bounded
    // identities. A changed identity is a new record, regardless of its slot.
    for fields in [
        &["config_scope", "group", "target"][..],
        &["id"][..],
        &["declaration_index", "node"][..],
        &["from", "to", "declaration"][..],
        &["name", "version", "source_pin"][..],
        &["source_pin"][..],
    ] {
        let values: Option<Vec<_>> = fields
            .iter()
            .map(|field| {
                let value = table.get(field)?.as_value()?;
                let value = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_integer().map(|value| value.to_string()))?;
                Some(format!("{field}\0{value}"))
            })
            .collect();
        if values.is_some() {
            return values;
        }
    }
    None
}

fn same_value(previous: &toml_edit::Value, next: &toml_edit::Value) -> bool {
    use toml_edit::Value;
    match (previous, next) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Float(a), Value::Float(b)) => a.value().to_bits() == b.value().to_bits(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Datetime(a), Value::Datetime(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_value(a, b))
        }
        (Value::InlineTable(a), Value::InlineTable(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_value(a, b)))
        }
        _ => false,
    }
}

fn retain_value_format(previous: &toml_edit::Value, next: &mut toml_edit::Value) {
    if same_value(previous, next) {
        *next = previous.clone();
        return;
    }
    *next.decor_mut() = previous.decor().clone();
    if let (toml_edit::Value::Array(previous), toml_edit::Value::Array(next)) = (previous, next) {
        // Declaration and build-requirement arrays contain scalar strings.
        // Match retained values by content so inserting a root does not move
        // another declaration's comment to a different package.
        let mut old =
            std::collections::BTreeMap::<&str, std::collections::VecDeque<&toml_edit::Value>>::new(
            );
        for value in previous.iter() {
            if let Some(text) = value.as_str() {
                old.entry(text).or_default().push_back(value);
            }
        }
        for value in next.iter_mut() {
            if let Some(previous) = value
                .as_str()
                .and_then(|text| old.get_mut(text))
                .and_then(std::collections::VecDeque::pop_front)
            {
                *value = previous.clone();
            }
        }
        next.set_trailing_comma(previous.trailing_comma());
        next.set_trailing(previous.trailing().clone());
    }
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
    fn inactive_models(targets: &[[u8; 32]]) -> PythonLockModel {
        use super::super::super::lock_model::{
            GroupKey, LockGraphInput, LockMarkerSemantics, RuntimePin,
        };
        let keys = targets
            .iter()
            .map(|target| GroupKey::new("", "shared-runtime", *target, Default::default()).unwrap())
            .collect::<Vec<_>>();
        let groups = keys
            .iter()
            .map(|key| LockGraphInput {
                key,
                semantics: LockMarkerSemantics::Current2026,
                runtime: RuntimePin::new([7; 32], 1, *key.target()).unwrap(),
                root_declarations: &["inactive==1.0; python_version < '1'"],
                roots: &[],
                nodes: &[],
                edges: &[],
            })
            .collect::<Vec<_>>();
        PythonLockModel::from_parts(&[], &groups, Default::default()).unwrap()
    }

    #[test]
    fn p3_projection_retains_matching_group_fields_and_comments_and_removes_old_targets() {
        let owner = tempfile::tempdir().unwrap();
        let path = owner.path().join("pakro.lock");
        let old_model = inactive_models(&[[3; 32], [4; 32]]).with_declaration_fingerprint([1; 32]);
        let old_projection = old_model
            .encode_projection(Default::default())
            .unwrap()
            .replacen("schema = 3", "schema = 3 # schema stays", 1)
            .replacen(
                &format!("declaration_fingerprint = \"{}\"", "01".repeat(32)),
                &format!(
                    "declaration_fingerprint = '{}' # fingerprint comment",
                    "01".repeat(32)
                ),
                1,
            )
            .replacen(
                "[[python.groups]]",
                "[[python.groups]] # retained group header",
                1,
            )
            .replacen(
                "group = \"shared-runtime\"",
                "group = 'shared-runtime' # retained group identity",
                1,
            )
            .replacen("roots = []", "roots = [ ] # no active roots", 1);
        let original = format!(
            "lockfile_version=4\n[future]\nvalue='stays' # foreign comment\n\n{old_projection}"
        );
        fs::write(&path, &original).unwrap();
        let root = open(&path);
        let model = inactive_models(&[[3; 32]]).with_declaration_fingerprint([2; 32]);
        let prepared = root
            .prepare_document(model.clone(), Default::default())
            .unwrap();
        let output = std::str::from_utf8(prepared.bytes()).unwrap();
        for expected in [
            "# schema stays",
            "# fingerprint comment",
            "# retained group header",
            "group = 'shared-runtime' # retained group identity",
            "roots = [ ] # no active roots",
            "value='stays' # foreign comment",
        ] {
            assert!(
                output.contains(expected),
                "missing retained syntax {expected}"
            );
        }
        assert!(!output.contains(&"04".repeat(32)));
        assert_eq!(
            super::super::decode_root(prepared.bytes(), Default::default())
                .unwrap()
                .unwrap(),
            model
        );
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn p3_damaged_or_unknown_python_projection_cannot_be_overwritten() {
        for original in [
            "lockfile_version=4\n[python]\nschema=2\npins=[]\ngroups=[]\nunknown='reject-me'\n",
            "lockfile_version=999\n[python]\nschema=2\npins=[]\ngroups=[]\n",
            "lockfile_version=4\n[python\nschema=2\n",
        ] {
            let owner = tempfile::tempdir().unwrap();
            let path = owner.path().join("pakro.lock");
            fs::write(&path, original).unwrap();
            assert!(
                BoundPythonLock::acquire(
                    &path,
                    Default::default(),
                    Instant::now() + Duration::from_secs(10)
                )
                .is_err()
            );
            assert_eq!(fs::read_to_string(path).unwrap(), original);
        }
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
