//! Publish one Conda Python payload through the ordinary language owner.
use super::lease::LanguageLease;
use super::publication::{publish_language, verify_conda_language};
use super::repository::{LanguageInstallSpec, LanguageRepository, LanguageReservation};
use crate::artifact_snapshot::VerifiedArtifact;
use crate::libraries::package::lease::PackageLease;
use crate::packages::conda_artifact::CondaTree;
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use rattler_conda_types::{Platform, package::PathType, prefix::Prefix};
use std::os::unix::fs::PermissionsExt;

/// The caller retains each verified package tree and its actual published
/// lease while the shared language payload and its runtime libraries are built.
pub(crate) async fn install_conda(
    context: &OperationContext,
    artifact: &VerifiedArtifact,
    python: &CondaTree,
    runtime_packages: &[(&PackageLease, &CondaTree)],
) -> Result<LanguageLease> {
    let record = python.record();
    let plan =
        crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
            record,
        )?;
    let spec = LanguageInstallSpec::from_plan(&plan, false)?;
    ensure!(
        artifact.filename() == plan.source().filename()
            && artifact.digest() == plan.source().digest(),
        "Conda Python tree differs from its fixed artifact"
    );
    let mut dependencies = Vec::with_capacity(runtime_packages.len());
    for &(lease, tree) in runtime_packages {
        lease.ensure_library_context(context)?;
        lease.check_binding()?;
        let package = &tree.record().package_record;
        let digest = pakro_store::Digest::Sha256(
            package
                .sha256
                .context("Conda runtime dependency lacks a digest")?
                .into(),
        );
        ensure!(
            lease.manifest().artifact()? == digest
                && lease.manifest().name() == package.name.as_normalized()
                && lease.manifest().version() == package.version.to_string()
                && lease.manifest().facts.source
                    == crate::libraries::package::conda::record_source(tree.record())?,
            "Conda runtime package differs from its published dependency"
        );
        dependencies.push(lease.clone());
    }
    let mut keys: Vec<_> = dependencies
        .iter()
        .map(|lease| lease.manifest().key())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let repository = LanguageRepository::open_writer(context)?;
    let mut owner = match repository.reserve_install(&spec)? {
        LanguageReservation::Existing(lease) => {
            ensure!(
                lease.spec().plan().conda_record() == spec.plan().conda_record()
                    && lease.runtime_dependencies() == keys.as_slice(),
                "installed Conda Python has different fixed runtime dependencies"
            );
            return Ok(lease);
        }
        LanguageReservation::Build(owner) => owner,
    };
    owner.check_artifact(artifact)?;
    let platform = record.package_record.subdir.parse::<Platform>()?;
    let final_prefix = owner
        .final_path()
        .to_str()
        .context("Conda language prefix is not UTF-8")?
        .to_owned();
    for tree in std::iter::once(python).chain(runtime_packages.iter().map(|(_, tree)| *tree)) {
        context.checkpoint()?;
        let scratch = tempfile::Builder::new()
            .prefix("conda-language-link-")
            .tempdir_in(context.requested_cache_root())?;
        let prefix = Prefix::create(scratch.path().join("prefix"))?;
        for entry in &tree.paths().paths {
            context.checkpoint()?;
            let relative = &entry.relative_path;
            if entry.path_type == PathType::Directory {
                owner.create_dir(relative, 0o755)?;
                continue;
            }
            let destination = prefix.path().join(relative);
            std::fs::create_dir_all(
                destination
                    .parent()
                    .context("Conda member parent is missing")?,
            )?;
            let mut linked_entry = entry.clone();
            if platform.is_osx()
                && linked_entry.sha256.is_none()
                && linked_entry
                    .prefix_placeholder
                    .as_ref()
                    .is_some_and(|placeholder| {
                        placeholder.file_mode == rattler_conda_types::package::FileMode::Binary
                    })
            {
                use sha2::Digest as _;
                use std::io::Read as _;
                let mut file = std::fs::File::open(tree.root().join(relative))?;
                let mut hash = sha2::Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    context.checkpoint()?;
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    hash.update(&buffer[..count]);
                }
                linked_entry.sha256 = Some(hash.finalize());
            }
            let result = rattler::install::link_file(
                &linked_entry,
                relative.to_owned(),
                tree.root(),
                &prefix,
                &final_prefix,
                true,
                false,
                false,
                platform,
                rattler::install::AppleCodeSignBehavior::Fail,
                filetime::FileTime::from_unix_time(0, 0),
                rattler::install::ExternalSymlinkPolicy::Deny,
            )?;
            ensure!(
                result.is_some(),
                "Conda package member disappeared during linking"
            );
            let metadata = std::fs::symlink_metadata(&destination)?;
            if metadata.file_type().is_symlink() {
                owner.create_symlink(relative, &std::fs::read_link(&destination)?)?;
            } else {
                ensure!(
                    metadata.is_file(),
                    "Conda linked member is not a regular file"
                );
                let mode = metadata.permissions().mode() & 0o755;
                let mut input = std::fs::File::open(&destination)?;
                let mut output = owner.create_file_exclusive(relative, mode)?;
                std::io::copy(&mut input, &mut output)?;
                output.finish()?;
            }
        }
    }
    publish_language(verify_conda_language(owner, artifact, dependencies).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rattler_conda_types::package::{
        ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType,
    };
    use rattler_conda_types::{PackageName, PackageRecord, RepoDataRecord, Version};
    use std::str::FromStr;

    #[test]
    fn conda_language_identity_tracks_the_python_artifact_and_target() {
        let mut package = PackageRecord::new(
            PackageName::from_str("python").unwrap(),
            Version::from_str("3.13.13").unwrap(),
            "h20e6be0_100_cp313".into(),
        );
        package.subdir = "osx-arm64".into();
        package.sha256 = Some([11u8; 32].into());
        let mut record = RepoDataRecord {
            package_record: package,
            identifier: DistArchiveIdentifier::new(
                ArchiveIdentifier {
                    name: "python".into(),
                    version: "3.13.13".into(),
                    build_string: "h20e6be0_100_cp313".into(),
                },
                DistArchiveType::Conda(CondaArchiveType::Conda),
            ),
            url: "https://example.invalid/python-3.13.13-h20e6be0_100_cp313.conda"
                .parse()
                .unwrap(),
            channel: Some("https://example.invalid/".into()),
        };
        let plan =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &record,
            )
            .unwrap();
        let key = LanguageInstallSpec::from_plan(&plan, false).unwrap().key();
        let same =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &record,
            )
            .unwrap();
        assert_eq!(
            key,
            LanguageInstallSpec::from_plan(&same, false).unwrap().key()
        );

        let mut alias = record.clone();
        alias.channel = Some("conda-forge".into());
        alias.url = "https://conda.anaconda.org/conda-forge/osx-arm64/python-3.13.13-h20e6be0_100_cp313.conda"
            .parse()
            .unwrap();
        let alias_plan =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &alias,
            )
            .unwrap();
        alias.channel = Some("https://conda.anaconda.org/conda-forge/".into());
        let canonical_plan =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &alias,
            )
            .unwrap();
        let channel_key = LanguageInstallSpec::from_plan(&alias_plan, false)
            .unwrap()
            .key();
        assert_eq!(
            channel_key,
            LanguageInstallSpec::from_plan(&canonical_plan, false)
                .unwrap()
                .key(),
            "equivalent channel spellings must reuse the same language identity"
        );
        assert_eq!(
            alias_plan.conda_record().unwrap().channel,
            canonical_plan.conda_record().unwrap().channel,
            "the retained source record must use the same canonical channel"
        );

        let mut other_channel = alias.clone();
        other_channel.channel = Some("https://mirror.example.invalid/other/".into());
        other_channel.url = "https://mirror.example.invalid/other/osx-arm64/python-3.13.13-h20e6be0_100_cp313.conda"
            .parse()
            .unwrap();
        let other_plan =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &other_channel,
            )
            .unwrap();
        let other_key = LanguageInstallSpec::from_plan(&other_plan, false)
            .unwrap()
            .key();
        assert_ne!(
            channel_key, other_key,
            "independent channels need distinct language identities"
        );
        assert_eq!(
            channel_key.target(),
            other_key.target(),
            "channel is not a target compatibility property"
        );

        let mut changed_dependencies = alias.clone();
        changed_dependencies.package_record.depends =
            vec!["openssl >=3".into(), "libffi >=3".into()];
        let changed_plan =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &changed_dependencies,
            )
            .unwrap();
        assert_eq!(
            channel_key,
            LanguageInstallSpec::from_plan(&changed_plan, false)
                .unwrap()
                .key(),
            "runtime dependency choices must not create another Python identity"
        );
        record.package_record.subdir = "linux-64".into();
        let other_target =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &record,
            )
            .unwrap();
        assert_ne!(
            key,
            LanguageInstallSpec::from_plan(&other_target, false)
                .unwrap()
                .key()
        );
        record.package_record.subdir = "osx-arm64".into();
        record.package_record.sha256 = Some([12u8; 32].into());
        let changed =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                &record,
            )
            .unwrap();
        assert_ne!(
            key,
            LanguageInstallSpec::from_plan(&changed, false)
                .unwrap()
                .key()
        );
    }
}
