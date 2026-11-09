//! Expected replay from an actual already-observed project, without replacing
//! that observation with another Simple response.
use super::*;

pub(crate) async fn acquire_selected_existing_registry_file<'lock>(
    slot: ExistingFileSlot<'lock>,
    selected: &SelectedRegistryFile<'_>,
    context: LockedReplayContext<'_>,
) -> LockedReplayResult<LockedRegistryArtifact<'lock>> {
    within_deadline(context.deadline, async {
        let limits = checked_limits(slot.pin(), &context)?;
        slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
        if !selected_matches(slot.pin().file(), selected) {
            return Err(LockedReplayError::FileIdentity);
        }
        let data = &selected.project.data;
        if !registry_matches(slot.pin().file(), context.project_session.registry) {
            return Err(LockedReplayError::ProjectContext);
        }
        let canonical = context
            .project_session
            .registry
            .project_url(data.project.name())
            .map_err(|_| LockedReplayError::ProjectContext)?;
        let transport = context.project_session.transport;
        let physical = transport
            .request_url(canonical.as_url())
            .map_err(|_| LockedReplayError::ProjectContext)?;
        deadline(context.deadline)?;
        let mut identity_bytes = 0;
        for value in [canonical.as_url().as_str(), physical.as_str()] {
            charge_identity(&mut identity_bytes, value, limits.max_identity_bytes)
                .map_err(|_| LockedReplayError::InputLimits)?;
        }
        let key = project_context_key(
            context.project_session.registry,
            context.project_session.auth_scope,
            &transport.fingerprint(),
            canonical.as_url(),
            &physical,
            data.project.name(),
        );
        if key != data.key {
            return Err(LockedReplayError::ProjectContext);
        }
        slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
        let advertised = page_digests(&data.project.files()[selected.file_index])?;
        if advertised
            .into_iter()
            .flatten()
            .any(|digest| conflicting_digest(slot.pin().digest(), digest))
        {
            return Err(LockedReplayError::HashConflict);
        }
        deadline(context.deadline)?;
        if let Some(artifact) = cached_replay_bytes(&slot, &context, limits)? {
            return Ok(LockedRegistryArtifact {
                slot,
                artifact,
                provenance: ReplayProvenance::SelectedCache {
                    project: Arc::clone(&selected.project.data),
                    file_index: selected.file_index,
                },
            });
        }
        if context.policy.network == NetworkPolicy::Offline {
            return Err(LockedReplayError::MissingInputs);
        }
        // This helper uses the very same observed Arc and file index. It has
        // no project-fetch branch and always verifies the old expected pin.
        download_selected(slot, selected, context, limits).await
    })
    .await
}

#[cfg(test)]
#[path = "selected/tests.rs"]
mod tests;
