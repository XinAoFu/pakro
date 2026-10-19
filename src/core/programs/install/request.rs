use std::fmt;

use crate::packages::PackageInstallRequest;
use crate::packages::binding::OwnedPackageRequest;
use crate::packages::request::{
    BarePackageBindingError, InstallArg, parse_install_arg, requests_from_bound,
    select_bare_package_runtime,
};
use crate::programs::shared::context::OperationContext;
use crate::programs::shared::errors::OperationError;
use crate::programs::shared::policy::RequestLimit;
use crate::programs::shared::types::{
    DeclarationEditIntent, DeclarationOperation, DeclarationTarget, RuntimeCandidate,
};
use crate::toolset::{ConfigScope, ToolSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallCommandIntent {
    Install,
    Use {
        target: DeclarationTarget,
    },
    Upgrade {
        declaration_target: Option<DeclarationTarget>,
    },
    DepsInstall,
    DepsAdd {
        target: DeclarationTarget,
    },
}

/// Facts supplied by the application boundary. This is deliberately not an
/// ObservedProjectInputs receipt and cannot authorize IO or a document commit.
pub struct InvocationInputs {
    pub arguments: Vec<String>,
    pub declarations: Vec<(PackageInstallRequest, ToolSource)>,
    pub runtime_candidates: Vec<RuntimeCandidate>,
    pub known_runtime_names: Vec<String>,
    pub scope: ConfigScope,
    pub context: OperationContext,
}

#[derive(Clone)]
pub struct InvocationSnapshot {
    pub(crate) arguments: Vec<String>,
    pub(crate) original_declarations: Vec<OwnedPackageRequest>,
    pub(crate) runtime_candidates: Vec<RuntimeCandidate>,
    pub(crate) known_runtime_names: Vec<String>,
    pub(crate) scope: ConfigScope,
    pub(crate) context: OperationContext,
}

impl InvocationSnapshot {
    pub fn from_inputs(inputs: InvocationInputs) -> Result<Self, OperationError> {
        validate_inputs(
            &inputs.context,
            &inputs.arguments,
            inputs.declarations.len(),
            inputs.runtime_candidates.len(),
        )?;
        let original_declarations = inputs
            .declarations
            .into_iter()
            .map(|(request, source)| OwnedPackageRequest { request, source })
            .collect();
        let snapshot = Self {
            arguments: inputs.arguments,
            original_declarations,
            runtime_candidates: inputs.runtime_candidates,
            known_runtime_names: inputs.known_runtime_names,
            scope: inputs.scope,
            context: inputs.context,
        };
        snapshot.context.checkpoint()?;
        Ok(snapshot)
    }
}

impl fmt::Debug for InvocationSnapshot {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("InvocationSnapshot")
            .field("argument_count", &self.arguments.len())
            .field("declaration_count", &self.original_declarations.len())
            .field("runtime_candidate_count", &self.runtime_candidates.len())
            .finish_non_exhaustive()
    }
}

pub struct InstallRequest {
    pub(crate) intent: InstallCommandIntent,
    pub(crate) invocation: InvocationSnapshot,
    /// Explicit package arguments. Existing declarations stay in the captured
    /// inputs for configuration precedence/owner validation at the context stage.
    pub(crate) packages: Vec<OwnedPackageRequest>,
    pub(crate) declaration_edit: Option<DeclarationEditIntent>,
}

impl InstallRequest {
    pub fn intent(&self) -> &InstallCommandIntent {
        &self.intent
    }
    pub fn context(&self) -> &OperationContext {
        &self.invocation.context
    }
    pub fn arguments(&self) -> &[String] {
        &self.invocation.arguments
    }
    pub fn scope(&self) -> ConfigScope {
        self.invocation.scope
    }
    pub fn original_declarations(
        &self,
    ) -> impl Iterator<Item = (&PackageInstallRequest, &ToolSource)> {
        self.invocation
            .original_declarations
            .iter()
            .map(|owned| (&owned.request, &owned.source))
    }
    pub fn packages(&self) -> impl Iterator<Item = (&PackageInstallRequest, &ToolSource)> {
        self.packages
            .iter()
            .map(|owned| (&owned.request, &owned.source))
    }
    pub fn declaration_edit(&self) -> Option<&DeclarationEditIntent> {
        self.declaration_edit.as_ref()
    }
}

impl fmt::Debug for InstallRequest {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("InstallRequest")
            .field("intent", &self.intent)
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

/// Pure request boundary. No source, filesystem or process capability is passed
/// here; a declaration edit describes intent and never writes the document.
pub fn normalize_intent(
    intent: InstallCommandIntent,
    invocation: InvocationSnapshot,
) -> Result<InstallRequest, OperationError> {
    validate_inputs(
        &invocation.context,
        &invocation.arguments,
        invocation.original_declarations.len(),
        invocation.runtime_candidates.len(),
    )?;
    let requested_edit = match &intent {
        InstallCommandIntent::Install | InstallCommandIntent::DepsInstall => None,
        InstallCommandIntent::Use { target } => Some((target, DeclarationOperation::SetSelection)),
        InstallCommandIntent::DepsAdd { target } => {
            Some((target, DeclarationOperation::AddDependency))
        }
        InstallCommandIntent::Upgrade { declaration_target } => declaration_target
            .as_ref()
            .map(|target| (target, DeclarationOperation::SetSelection)),
    };
    let declaration_edit = if let Some((target, operation)) = requested_edit {
        if invocation.context.policy().locked {
            return Err(OperationError::LockedDeclarationEdit);
        }
        if !invocation.context.policy().writes.declarations {
            return Err(OperationError::DeclarationWriteDenied);
        }
        if !target.source.path().is_some_and(|path| path.is_absolute())
            || target.field_path.is_empty()
            || target.field_path.iter().any(String::is_empty)
        {
            return Err(OperationError::InvalidDeclarationTarget);
        }
        Some(DeclarationEditIntent {
            target: target.clone(),
            operation,
        })
    } else {
        None
    };

    let candidates: Vec<_> = invocation
        .runtime_candidates
        .iter()
        .filter(|candidate| matches!(candidate.runtime_name.as_str(), "python" | "node"))
        .collect();
    let mut packages = Vec::new();
    for (index, argument) in invocation.arguments.iter().enumerate() {
        invocation.context.checkpoint()?;
        let invalid = |_| OperationError::InvalidArgument { index };
        let requests = match parse_install_arg(argument).map_err(invalid)? {
            InstallArg::Bound {
                tool,
                tool_version,
                packages,
            } => requests_from_bound(&tool, tool_version.as_deref(), &packages).map_err(invalid)?,
            InstallArg::Bare { name, version }
                if !invocation.known_runtime_names.contains(&name) && version.is_some() =>
            {
                let candidate =
                    select_bare_package_runtime(&candidates).map_err(|error| match error {
                        BarePackageBindingError::MissingRuntime => {
                            OperationError::MissingRuntimeBinding
                        }
                        BarePackageBindingError::AmbiguousRuntime { candidate_count } => {
                            OperationError::AmbiguousRuntimeBinding { candidate_count }
                        }
                    })?;
                let pins = [(name, version.expect("the match guard requires a version"))];
                requests_from_bound(
                    &candidate.runtime_name,
                    Some(&candidate.runtime_version),
                    &pins,
                )
                .map_err(invalid)?
            }
            // Preserve language and unversioned arguments in the snapshot;
            // later context/source selection owns their interpretation.
            InstallArg::Bare { .. } => Vec::new(),
        };
        packages.extend(requests.into_iter().map(|request| OwnedPackageRequest {
            request,
            source: ToolSource::Argument,
        }));
    }
    invocation.context.checkpoint()?;
    Ok(InstallRequest {
        intent,
        invocation,
        packages,
        declaration_edit,
    })
}

fn validate_inputs(
    context: &OperationContext,
    arguments: &[String],
    declaration_count: usize,
    runtime_candidate_count: usize,
) -> Result<(), OperationError> {
    context.checkpoint()?;
    let budget = context.policy().budget;
    for (actual, allowed, limit) in [
        (
            arguments.len(),
            budget.max_arguments,
            RequestLimit::Arguments,
        ),
        (
            declaration_count,
            budget.max_declarations,
            RequestLimit::Declarations,
        ),
        (
            runtime_candidate_count,
            budget.max_runtime_candidates,
            RequestLimit::RuntimeCandidates,
        ),
    ] {
        if actual > allowed {
            return Err(OperationError::ResourceLimit(limit));
        }
    }
    let mut remaining_bytes = budget.max_total_argument_bytes;
    for argument in arguments {
        context.checkpoint()?;
        remaining_bytes = remaining_bytes
            .checked_sub(argument.len())
            .ok_or(OperationError::ResourceLimit(RequestLimit::ArgumentBytes))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
