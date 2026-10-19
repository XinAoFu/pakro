//! Descriptive outcomes of a mutating operation, not transaction capabilities.
//!
//! Subjects retain the caller's descriptive references. This layer does not
//! invent shared-object identities, create commit receipts, or authorize
//! publication, deletion, cancellation, or recovery. Query results stay separate.

/// What the operation can report about its observed commit state.
/// RecoveryRequired deliberately does not imply either a successful commit or
/// a successful rollback; the later transaction owner must resolve that state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    NoChange,
    PreparedOnly,
    Committed,
    CommittedNeedsFinalization,
    NotCommitted,
    RecoveryRequired,
}

/// Descriptions of observed effects. `S` is a caller-supplied subject reference,
/// not an identity format or a publication/removal permit owned by this module.
pub struct OperationEffects<S> {
    pub added: Vec<S>,
    pub reused: Vec<S>,
    pub associations_changed: Vec<S>,
    pub retained: Vec<S>,
    pub removed: Vec<S>,
}

/// A report keeps commit facts separate from any later failure or cancellation.
/// Warning text is supplied for presentation by the caller; this type does not
/// derive Debug or serialize raw subject values or diagnostics automatically.
pub struct OperationReport<S> {
    state: OperationState,
    effects: OperationEffects<S>,
    warnings: Vec<String>,
}

impl<S> OperationReport<S> {
    pub fn new(state: OperationState, effects: OperationEffects<S>, warnings: Vec<String>) -> Self {
        Self {
            state,
            effects,
            warnings,
        }
    }

    pub fn state(&self) -> OperationState {
        self.state
    }

    pub fn effects(&self) -> &OperationEffects<S> {
        &self.effects
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Attach why execution stopped without replacing the already observed
    /// commit facts. This wrapper neither cancels a process nor runs recovery.
    pub fn with_termination<E>(
        self,
        termination: OperationTermination<E>,
    ) -> InterruptedOperation<S, E> {
        InterruptedOperation {
            report: self,
            termination,
        }
    }
}

pub enum OperationTermination<E> {
    Failed(E),
    Cancelled,
}

/// An error path retains its full report instead of erasing a commit point.
/// The cause type remains owned by the caller; no conversion to a message or
/// raw Debug output is required to carry it alongside the report.
pub struct InterruptedOperation<S, E> {
    report: OperationReport<S>,
    termination: OperationTermination<E>,
}

impl<S, E> InterruptedOperation<S, E> {
    pub fn report(&self) -> &OperationReport<S> {
        &self.report
    }

    pub fn termination(&self) -> &OperationTermination<E> {
        &self.termination
    }
}

#[cfg(test)]
mod tests;
