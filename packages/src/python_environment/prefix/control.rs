//! Application control scope over the shared anchored IO kernel.
//! Journal, generation and ownership semantics remain in this application.

use super::*;

pub(crate) mod lease;
pub(crate) mod ownership;
pub(crate) mod scan;

pub(crate) use pakro_store::control::{ControlLimits, ControlNamespace, ControlSnapshot};

pub(crate) struct StoreControl<'a> {
    store: &'a EnvironmentStore,
    limits: ControlLimits,
    root: File,
    namespaces: [File; 4],
    core: pakro_store::control::WriterControl<'a>,
}

impl EnvironmentStore {
    pub(crate) fn with_control<T>(
        &self,
        limits: ControlLimits,
        action: impl FnOnce(&mut StoreControl<'_>) -> Result<T>,
    ) -> Result<T> {
        self.check_store_binding()?;
        let core = self.kernel.control(limits)?;
        let root = core.control_handle().try_clone()?;
        let namespaces = [
            core.namespace_handle(ControlNamespace::Catalog)
                .try_clone()?,
            core.namespace_handle(ControlNamespace::Journals)
                .try_clone()?,
            core.namespace_handle(ControlNamespace::Seals).try_clone()?,
            core.namespace_handle(ControlNamespace::Ownership)
                .try_clone()?,
        ];
        let mut control = StoreControl {
            store: self,
            limits,
            root,
            namespaces,
            core,
        };
        control.check_binding()?;
        let result = action(&mut control);
        if result.is_ok() {
            control.check_binding()?;
        }
        result
    }
}

impl StoreControl<'_> {
    pub(crate) fn store_identity(&self) -> Result<(u64, u64)> {
        self.core.store_identity()
    }
    pub(crate) fn list_names(
        &self,
        namespace: ControlNamespace,
        max_entries: usize,
        max_total_name_bytes: usize,
    ) -> Result<Vec<String>> {
        self.core
            .list_names(namespace, max_entries, max_total_name_bytes)
    }
    pub(crate) fn read_bounded(
        &self,
        namespace: ControlNamespace,
        name: &str,
        max_bytes: u64,
    ) -> Result<Option<ControlSnapshot>> {
        self.core.read_bounded(namespace, name, max_bytes)
    }
    pub(crate) fn read(
        &self,
        namespace: ControlNamespace,
        name: &str,
    ) -> Result<Option<ControlSnapshot>> {
        self.core.read(namespace, name)
    }
    pub(crate) fn replace(
        &mut self,
        namespace: ControlNamespace,
        name: &str,
        expected: Option<&ControlSnapshot>,
        bytes: &[u8],
    ) -> Result<ControlSnapshot> {
        self.core.replace(namespace, name, expected, bytes)
    }
    pub(crate) fn remove(&mut self, expected: &ControlSnapshot) -> Result<()> {
        self.core.remove(expected)
    }
    fn validate_name(&self, name: &str) -> Result<()> {
        self.core.validate_name(name)
    }
    fn validate_file(&self, file: &File) -> Result<()> {
        self.core.validate_file(file)
    }
    fn check_binding(&self) -> Result<()> {
        self.store.check_store_binding()?;
        self.core.check_binding()
    }
}

#[cfg(test)]
#[path = "control/tests.rs"]
mod tests;
