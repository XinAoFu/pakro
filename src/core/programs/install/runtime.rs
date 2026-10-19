//! Prepare a shared language using one frozen operation context.

use std::future::Future;

use eyre::Result;

use crate::artifact_snapshot::VerifiedArtifact;
use crate::libraries::language::lease::LanguageLease;
use crate::libraries::language::publication::{publish_language, verify_language};
use crate::libraries::language::repository::LanguageInstallSpec;
use crate::libraries::language::repository::{LanguageRepository, LanguageReservation};
use crate::programs::shared::context::OperationContext;

/// Revalidation is considered only while an existing instance's SH is held.
pub(crate) async fn prepare_language_with_revalidation<S, F, Fut>(
    spec: &LanguageInstallSpec,
    context: &OperationContext,
    revalidate: S,
    acquire: F,
) -> Result<LanguageLease>
where
    S: FnOnce() -> Result<bool>,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<VerifiedArtifact>>,
{
    let cancelled_on_drop = context.cancellation_token().drop_guard();
    let result = prepare_language_inner(spec, context, revalidate, acquire).await;
    // Normal errors permit read-only inspection; abandonment revokes workers.
    cancelled_on_drop.disarm();
    result
}

/// The acquisition closure is invoked only after the shared key is reserved.
pub(crate) async fn prepare_language<F, Fut>(
    spec: &LanguageInstallSpec,
    context: &OperationContext,
    acquire: F,
) -> Result<LanguageLease>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<VerifiedArtifact>>,
{
    prepare_language_with_revalidation(spec, context, || Ok(false), acquire).await
}

async fn acquire_for_language<F, Fut>(
    spec: &LanguageInstallSpec,
    context: &OperationContext,
    acquire: F,
) -> Result<VerifiedArtifact>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<VerifiedArtifact>>,
{
    context.checkpoint()?;
    let cancellation = context.cancellation_token();
    let artifact = tokio::select! {
        _=cancellation.cancelled()=>return Err(eyre::eyre!("language preparation cancelled")),
        result=tokio::time::timeout_at(tokio::time::Instant::from_std(context.policy().deadline),acquire())=>result.map_err(|_|eyre::eyre!("language preparation timed out"))??,
    };
    context.checkpoint()?;
    eyre::ensure!(
        artifact.digest() == spec.plan().source().digest()
            && artifact.filename() == spec.plan().source().filename(),
        "verified runtime artifact differs from the selected language spec"
    );
    Ok(artifact)
}

async fn prepare_language_inner<S, F, Fut>(
    spec: &LanguageInstallSpec,
    context: &OperationContext,
    revalidate: S,
    acquire: F,
) -> Result<LanguageLease>
where
    S: FnOnce() -> Result<bool>,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<VerifiedArtifact>>,
{
    context.checkpoint()?;
    let operation = context.clone();
    let requested = spec.clone();
    let reservation = tokio::task::spawn_blocking(move || {
        operation.checkpoint()?;
        // Read-only callers may reuse existing content without gaining a writer.
        if let Some(existing) =
            LanguageRepository::open_readonly(&operation)?.acquire_existing(&requested)?
        {
            return Ok::<_, eyre::Report>(LanguageReservation::Existing(existing));
        }
        LanguageRepository::open_writer(&operation)?.reserve_install(&requested)
    })
    .await??;
    context.checkpoint()?;
    let mut owner = match reservation {
        LanguageReservation::Existing(existing) => {
            if revalidate()? {
                let _artifact = acquire_for_language(spec, context, acquire).await?;
                existing.check_binding()?;
            }
            context.checkpoint()?;
            return Ok(existing);
        }
        LanguageReservation::Build(owner) => owner,
    };
    let mut artifact = acquire_for_language(spec, context, acquire).await?;
    let operation = context.clone();
    let requested = spec.clone();
    let runtime = tokio::runtime::Handle::current();
    // Tar decoding and complete inventory are synchronous, bounded IO. Keep
    // them off the runtime thread so its cancellation tasks remain runnable.
    tokio::task::spawn_blocking(move || {
        runtime.block_on(async move {
            operation.checkpoint()?;
            let receipt = crate::plugins::core::python::native::extract::install_into_owner(
                requested.plan(),
                &mut artifact,
                &mut owner,
                requested.patch_sysconfig(),
            )
            .await?;
            operation.checkpoint()?;
            let verified = verify_language(owner, receipt).await?;
            operation.checkpoint()?;
            publish_language(verified)
        })
    })
    .await?
}

#[cfg(all(test, unix, feature = "self_update"))]
mod tests;
