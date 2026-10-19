//! Bind description-only requests to one current project observation.

use super::request::InstallRequest;
use crate::config::config_file::ConfigDocumentFormat;
use crate::libraries::project::declarations::{
    DeclarationChange, DeclarationEditError, ProposedDeclarationEdit, prepare_edit,
};
use crate::libraries::project::snapshot::{
    ObservedProjectInputs, ProjectObservationError, ReadScope,
};
use crate::packages::request::{InstallArg, parse_install_arg};
use crate::programs::shared::context::OperationContext;
use crate::programs::shared::errors::OperationError;
use crate::programs::shared::types::{DeclarationOperation, RuntimeCandidate};
use crate::toolset::{ConfigScope, ToolSource};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContextBindingError {
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error(transparent)]
    Observation(#[from] ProjectObservationError),
    #[error(transparent)]
    Declaration(#[from] DeclarationEditError),
    #[error("request and observation do not belong to the same invocation")]
    InvocationMismatch,
    #[error("request descriptions do not match the observed declarations")]
    DeclarationMismatch,
    #[error("declaration edit targets another project or field")]
    InvalidTarget,
}

pub(crate) struct BoundInstallContext<'input> {
    request: &'input InstallRequest,
    observed: &'input ObservedProjectInputs,
}

pub(crate) fn capture_context<'input>(
    request: &'input InstallRequest,
    observed: &'input ObservedProjectInputs,
    operation: &OperationContext,
) -> Result<BoundInstallContext<'input>, ContextBindingError> {
    operation.checkpoint()?;
    if !request.context().same_invocation(operation)
        || observed.deadline() != operation.policy().deadline
        || observed.cancellation() != &operation.cancellation_token()
        || observed.cli_arguments() != request.arguments()
        || !matches!(
            (request.scope(), observed.scope()),
            (ConfigScope::All, ReadScope::Project)
                | (ConfigScope::LocalOnly, ReadScope::LocalOnly)
                | (ConfigScope::GlobalOnly, ReadScope::GlobalOnly)
        )
    {
        return Err(ContextBindingError::InvocationMismatch);
    }
    observed.recheck()?;
    if let Some(edit) = request.declaration_edit() {
        let target = edit
            .target
            .source
            .path()
            .ok_or(ContextBindingError::InvalidTarget)?;
        if !observed.contains_edit_target(target) {
            return Err(ContextBindingError::InvalidTarget);
        }
        match (&edit.operation, edit.target.field_path.as_slice()) {
            (DeclarationOperation::SetSelection, [field] | [field, _]) if field == "tools" => {}
            (DeclarationOperation::AddDependency, [field] | [field, _]) if field == "packages" => {}
            _ => return Err(ContextBindingError::InvalidTarget),
        }
    }
    // The old request snapshot contains descriptions only. Do not promote a
    // cached Config projection merely because the filenames happen to agree.
    let actual: Vec<_> = observed
        .declarations()
        .flat_map(|(path, declarations)| {
            declarations
                .packages
                .iter()
                .map(move |package| (package, ToolSource::PakroToml(path.to_path_buf())))
        })
        .collect();
    let described: Vec<_> = request.original_declarations().collect();
    if actual.len() != described.len()
        || actual.iter().zip(&described).any(
            |((actual, source), (description, described_source))| {
                actual != description || source != *described_source
            },
        )
    {
        return Err(ContextBindingError::DeclarationMismatch);
    }
    let candidates = effective_runtime_candidates(observed, operation)?;
    if request
        .invocation
        .runtime_candidates
        .iter()
        .any(|candidate| !candidates.contains(candidate))
    {
        return Err(ContextBindingError::DeclarationMismatch);
    }
    // Reuse R01 normalization against the observed effective candidates. A
    // caller cannot conceal ambiguity by omitting one candidate in a DTO.
    let mut invocation = request.invocation.clone();
    invocation.runtime_candidates = candidates;
    let checked = super::request::normalize_intent(request.intent().clone(), invocation)?;
    if checked
        .packages
        .iter()
        .map(|owned| &owned.request)
        .ne(request.packages.iter().map(|owned| &owned.request))
    {
        return Err(ContextBindingError::DeclarationMismatch);
    }
    operation.checkpoint()?;
    Ok(BoundInstallContext { request, observed })
}

impl BoundInstallContext<'_> {
    pub(crate) fn recheck(&self) -> Result<(), ContextBindingError> {
        self.request.context().checkpoint()?;
        self.observed.recheck()?;
        Ok(())
    }
    pub(crate) fn prepare_declaration_edit(
        &self,
    ) -> Result<Option<ProposedDeclarationEdit<'_>>, ContextBindingError> {
        self.recheck()?;
        let Some(edit) = self.request.declaration_edit() else {
            return Ok(None);
        };
        let path = edit
            .target
            .source
            .path()
            .ok_or(ContextBindingError::InvalidTarget)?;
        let mut changes = Vec::new();
        match edit.operation {
            DeclarationOperation::SetSelection => {
                let mut selections = indexmap::IndexMap::<String, Vec<String>>::new();
                for argument in self.request.arguments() {
                    let parsed = parse_install_arg(argument)
                        .map_err(|_| ContextBindingError::InvalidTarget)?;
                    let (name, version) = match parsed {
                        InstallArg::Bare {
                            name,
                            version: Some(version),
                        } if self.request.invocation.known_runtime_names.contains(&name) => {
                            (name, version)
                        }
                        InstallArg::Bound {
                            tool,
                            tool_version: Some(version),
                            packages,
                        } if packages.is_empty() => (tool, version),
                        _ => return Err(ContextBindingError::InvalidTarget),
                    };
                    if edit
                        .target
                        .field_path
                        .get(1)
                        .is_some_and(|field| field != &name)
                    {
                        return Err(ContextBindingError::InvalidTarget);
                    }
                    selections.entry(name).or_default().push(version);
                }
                changes.extend(
                    selections
                        .into_iter()
                        .map(|(key, versions)| DeclarationChange::SetTool { key, versions }),
                );
            }
            DeclarationOperation::AddDependency => {
                let requests: Vec<_> = self
                    .request
                    .packages()
                    .map(|(request, _)| request.clone())
                    .collect();
                if requests.is_empty()
                    || requests.iter().any(|request| {
                        edit.target
                            .field_path
                            .get(1)
                            .is_some_and(|group| group != &request.group_key())
                    })
                {
                    return Err(ContextBindingError::InvalidTarget);
                }
                changes.push(DeclarationChange::AddPinnedPackages { requests });
            }
        }
        if changes.is_empty() {
            return Err(ContextBindingError::InvalidTarget);
        }
        let document = prepare_edit(self.observed, path, &changes)?;
        self.recheck()?;
        Ok(Some(document))
    }
}

fn effective_runtime_candidates(
    observed: &ObservedProjectInputs,
    operation: &OperationContext,
) -> Result<Vec<RuntimeCandidate>, ContextBindingError> {
    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    let mut push = |candidate| -> Result<(), ContextBindingError> {
        operation.checkpoint()?;
        if candidates.len() >= operation.policy().budget.max_runtime_candidates {
            return Err(OperationError::ResourceLimit(
                crate::programs::RequestLimit::RuntimeCandidates,
            )
            .into());
        }
        candidates.push(candidate);
        Ok(())
    };
    // Preserve the existing scope contract: LocalOnly excludes shell selections.
    if observed.scope() != ReadScope::LocalOnly {
        for (key, value) in observed.environment() {
            let Some(name) = crate::toolset::tool_from_env_var_name(key) else {
                continue;
            };
            if !matches!(name.as_str(), "python" | "node") {
                continue;
            }
            seen.insert(name.clone());
            for version in value.split_whitespace() {
                push(RuntimeCandidate {
                    runtime_name: name.clone(),
                    runtime_version: version.to_owned(),
                    source: ToolSource::Environment(key.clone(), value.clone()),
                })?;
            }
        }
    }
    for (path, declarations) in observed.declarations() {
        let mut names = std::collections::HashSet::new();
        for tool in &declarations.tools {
            let key = crate::args::split_bracketed_opts(&tool.name)
                .map_or(tool.name.as_str(), |(name, _)| name);
            let name = crate::backend::unalias_backend(key);
            if seen.contains(name.as_ref()) {
                continue;
            }
            names.insert(name.to_string());
            if tool.selector != "version" || !matches!(name.as_ref(), "python" | "node") {
                continue;
            }
            let source = match observed.document_format(path) {
                Some(ConfigDocumentFormat::ToolVersions) => {
                    ToolSource::ToolVersions(path.to_path_buf())
                }
                _ => ToolSource::PakroToml(path.to_path_buf()),
            };
            push(RuntimeCandidate {
                runtime_name: name.to_string(),
                runtime_version: tool.version.clone(),
                source,
            })?;
        }
        seen.extend(names);
    }
    Ok(candidates)
}

#[cfg(all(test, unix))]
mod tests;
