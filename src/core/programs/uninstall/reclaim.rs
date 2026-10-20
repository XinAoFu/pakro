//! Acquires actual object protection before rechecking and removing content.
use crate::programs::OperationContext;
use eyre::Result;

pub(crate) fn language(
    context: &OperationContext,
    version: &str,
    instance: Option<&str>,
    dry_run: bool,
) -> Result<String> {
    let target = super::select::language(context, version, instance)?;
    super::reachability::ensure_unreferenced(context, target.key().fingerprint())?;
    if !dry_run {
        crate::libraries::language::removal::remove(context, &target)?;
    }
    Ok(target.key().filename())
}

pub(crate) fn package(
    context: &OperationContext,
    request: &crate::packages::PackageInstallRequest,
    instance: Option<&str>,
    dry_run: bool,
) -> Result<String> {
    let target = super::select::package(context, request, instance)?;
    super::reachability::ensure_unreferenced(context, target.key())?;
    if !dry_run {
        crate::libraries::package::removal::remove(context, &target)?;
    }
    Ok(format!("package-{}", hex::encode(target.key())))
}
