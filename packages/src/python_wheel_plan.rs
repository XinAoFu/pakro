//! Whole-graph wheel destinations from explicit inspection facts and paths.
//!
//! These values neither authenticate archives nor verify a runtime or prefix.
//! C prepares every script transformation and generated file before writes;
//! D separately supplies a verified unpublished prefix to the materializer.
//! The complete type means all required writes are planned; it never grants
//! filesystem, source-authentication or environment-publication authority.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::path::{Component, Path, PathBuf};

use eyre::{ContextCompat, Result, ensure};

use super::python_wheel::{WheelManifest, WheelMember, WheelMemberRole, WheelScheme};

const SCHEMES: [WheelScheme; 5] = [
    WheelScheme::Purelib,
    WheelScheme::Platlib,
    WheelScheme::Headers,
    WheelScheme::Scripts,
    WheelScheme::Data,
];

#[derive(Debug, Clone)]
pub(crate) struct WheelInstallScheme {
    final_prefix: PathBuf,
    roots: BTreeMap<WheelScheme, PathBuf>,
}

impl WheelInstallScheme {
    /// Pure shape validation only; callers supply the selected runtime's real
    /// final-prefix paths, with physical scheme aliases already resolved.
    pub(crate) fn new(
        final_prefix: PathBuf,
        roots: BTreeMap<WheelScheme, PathBuf>,
    ) -> Result<Self> {
        ensure!(
            roots.len() == SCHEMES.len() && SCHEMES.iter().all(|key| roots.contains_key(key)),
            "wheel scheme must supply all five roots"
        );
        let final_prefix = absolute_path(&final_prefix, "scheme final prefix")?;
        let mut normalized = BTreeMap::new();
        for (key, root) in roots {
            let root = absolute_path(&root, "scheme root")?;
            ensure!(
                root.starts_with(&final_prefix),
                "wheel scheme root escapes final prefix"
            );
            normalized.insert(key, root);
        }
        Ok(Self {
            final_prefix,
            roots: normalized,
        })
    }

    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }

    pub(crate) fn root(&self, scheme: WheelScheme) -> Option<&Path> {
        self.roots.get(&scheme).map(PathBuf::as_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WheelOwner {
    name: String,
    version: String,
}

impl WheelOwner {
    pub(crate) fn from_manifest(manifest: &WheelManifest) -> Self {
        Self {
            name: manifest.name().into(),
            version: manifest.version().into(),
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WheelWriteOperation {
    Copy,
    RewritePythonShebang,
    GenerateEntryPoint,
    GenerateRecord,
    GenerateInstaller,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WheelGeneratedTarget {
    Scripts(String),
    InstalledRecord,
    Installer,
}

#[derive(Debug, Clone)]
pub(crate) struct WheelGeneratedIntent {
    pub owner: WheelOwner,
    pub target: WheelGeneratedTarget,
    pub operation: WheelWriteOperation,
    pub sha256: [u8; 32],
    pub size: u64,
    pub mode: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct WheelTransformIntent {
    pub owner: WheelOwner,
    pub archive_index: usize,
    pub operation: WheelWriteOperation,
    pub sha256: [u8; 32],
    pub size: u64,
    pub mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WheelReservedKind {
    File,
    Directory,
    ExclusiveTree,
}

#[derive(Debug, Clone)]
pub(crate) struct WheelReservedDestination {
    pub path: PathBuf,
    pub kind: WheelReservedKind,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WheelPlanLimits {
    pub max_wheels: usize,
    pub max_sources: usize,
    pub max_files: usize,
    pub max_directories: usize,
    pub max_total_bytes: u64,
    pub max_path_bytes: usize,
}

impl Default for WheelPlanLimits {
    fn default() -> Self {
        Self {
            max_wheels: 16_384,
            max_sources: 1_048_576,
            max_files: 1_048_576,
            max_directories: 1_048_576,
            max_total_bytes: 512 * 1024 * 1024 * 1024,
            max_path_bytes: 16_384,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WheelPlanSource {
    Archive {
        owner: WheelOwner,
        archive_index: usize,
        archive_path: String,
        raw_sha256: [u8; 32],
        raw_size: u64,
    },
    Generated {
        owner: WheelOwner,
        target: WheelGeneratedTarget,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WheelFilePlan {
    destination: PathBuf,
    sha256: [u8; 32],
    size: u64,
    mode: u32,
    operation: WheelWriteOperation,
    owners: Vec<WheelOwner>,
    sources: Vec<WheelPlanSource>,
}

impl WheelFilePlan {
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn mode(&self) -> u32 {
        self.mode
    }
    pub(crate) fn operation(&self) -> WheelWriteOperation {
        self.operation
    }
    pub(crate) fn owners(&self) -> &[WheelOwner] {
        &self.owners
    }
    pub(crate) fn sources(&self) -> &[WheelPlanSource] {
        &self.sources
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WheelDirectoryPlan {
    destination: PathBuf,
    owners: Vec<WheelOwner>,
}

impl WheelDirectoryPlan {
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }
    pub(crate) fn owners(&self) -> &[WheelOwner] {
        &self.owners
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WheelMetadataReservation {
    owner: WheelOwner,
    target: WheelGeneratedTarget,
    destination: PathBuf,
}

impl WheelMetadataReservation {
    pub(crate) fn owner(&self) -> &WheelOwner {
        &self.owner
    }
    pub(crate) fn target(&self) -> &WheelGeneratedTarget {
        &self.target
    }
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WheelGraphPlan {
    final_prefix: PathBuf,
    files: Vec<WheelFilePlan>,
    directories: Vec<WheelDirectoryPlan>,
    pending_metadata: Vec<WheelMetadataReservation>,
}

impl WheelGraphPlan {
    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }
    pub(crate) fn files(&self) -> &[WheelFilePlan] {
        &self.files
    }
    pub(crate) fn directories(&self) -> &[WheelDirectoryPlan] {
        &self.directories
    }
    pub(crate) fn pending_metadata(&self) -> &[WheelMetadataReservation] {
        &self.pending_metadata
    }

    /// Completeness of planned writes only; never prefix/source authority.
    pub(crate) fn require_complete(self) -> Result<CompleteWheelGraphPlan> {
        ensure!(
            self.pending_metadata.is_empty(),
            "wheel metadata generation is incomplete"
        );
        Ok(CompleteWheelGraphPlan { plan: self })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CompleteWheelGraphPlan {
    plan: WheelGraphPlan,
}

impl CompleteWheelGraphPlan {
    pub(crate) fn plan(&self) -> &WheelGraphPlan {
        &self.plan
    }
}

pub(crate) fn plan_wheel_graph(
    manifests: &[WheelManifest],
    scheme: &WheelInstallScheme,
    generated: &[WheelGeneratedIntent],
    transformed: &[WheelTransformIntent],
    reserved: &[WheelReservedDestination],
    limits: WheelPlanLimits,
) -> Result<WheelGraphPlan> {
    preflight(manifests, generated, transformed, reserved, limits)?;
    check_path_limit(scheme.final_prefix(), limits)?;
    for root in scheme.roots.values() {
        check_path_limit(root, limits)?;
    }
    let scheme = WheelInstallScheme::new(scheme.final_prefix.clone(), scheme.roots.clone())?;
    let states = manifest_states(manifests, &scheme, limits)?;
    let reserved = reserved
        .iter()
        .map(|entry| {
            check_path_limit(&entry.path, limits)?;
            let path = absolute_path(&entry.path, "reserved destination")?;
            ensure!(
                path.starts_with(scheme.final_prefix()),
                "wheel reserved destination escapes final prefix"
            );
            Ok(WheelReservedDestination {
                path,
                kind: entry.kind,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let transforms = transform_map(transformed, &states)?;
    let generated = generated_map(generated, &states, limits)?;
    let mut layout = Layout::new(&scheme, &states, &reserved, limits)?;
    for (owner, state) in &states {
        for target in [
            WheelGeneratedTarget::InstalledRecord,
            WheelGeneratedTarget::Installer,
        ] {
            layout.reserve_metadata(owner, state.metadata_path(&target))?;
        }
    }
    for (owner, state) in &states {
        for member in state.manifest.members() {
            let destination = state.destination(member, &scheme)?;
            check_path_limit(&destination, limits)?;
            let record = state.metadata_path(&WheelGeneratedTarget::InstalledRecord);
            let installer = state.metadata_path(&WheelGeneratedTarget::Installer);
            if member.role() == WheelMemberRole::Record {
                ensure!(
                    destination == record,
                    "wheel metadata RECORD source is inconsistent"
                );
                continue;
            }
            // This one fixed owner slot is replaced or left pending for C.
            if destination == installer {
                continue;
            }
            let (sha256, size, mode, operation) = if member.scheme() == WheelScheme::Scripts {
                let intent = transforms
                    .get(&(owner.clone(), member.archive_index()))
                    .context("wheel script source is missing a transform intent")?;
                (intent.sha256, intent.size, intent.mode, intent.operation)
            } else {
                (
                    *member.observed_sha256(),
                    member.size(),
                    member_mode(member),
                    WheelWriteOperation::Copy,
                )
            };
            layout.insert_file(
                WheelFilePlan {
                    destination,
                    sha256,
                    size,
                    mode,
                    operation,
                    owners: vec![owner.clone()],
                    sources: vec![WheelPlanSource::Archive {
                        owner: owner.clone(),
                        archive_index: member.archive_index(),
                        archive_path: member.archive_path().into(),
                        raw_sha256: *member.observed_sha256(),
                        raw_size: member.size(),
                    }],
                },
                None,
            )?;
        }
    }
    for ((owner, target), intent) in generated {
        let state = states
            .get(&owner)
            .context("wheel generated owner is missing")?;
        let destination = match &target {
            WheelGeneratedTarget::Scripts(name) => scheme
                .root(WheelScheme::Scripts)
                .context("wheel scripts scheme is missing")?
                .join(name),
            _ => state.metadata_path(&target),
        };
        let slot = if matches!(target, WheelGeneratedTarget::Scripts(_)) {
            None
        } else {
            Some((owner.clone(), target.clone()))
        };
        layout.insert_file(
            WheelFilePlan {
                destination,
                sha256: intent.sha256,
                size: intent.size,
                mode: intent.mode,
                operation: intent.operation,
                owners: vec![owner.clone()],
                sources: vec![WheelPlanSource::Generated { owner, target }],
            },
            slot,
        )?;
    }
    Ok(layout.finish())
}

fn preflight(
    manifests: &[WheelManifest],
    generated: &[WheelGeneratedIntent],
    transformed: &[WheelTransformIntent],
    reserved: &[WheelReservedDestination],
    limits: WheelPlanLimits,
) -> Result<()> {
    ensure!(
        limits.max_wheels > 0
            && limits.max_sources > 0
            && limits.max_files > 0
            && limits.max_directories > 0
            && limits.max_total_bytes > 0
            && limits.max_path_bytes > 0,
        "wheel graph limit values must be positive"
    );
    ensure!(
        manifests.len() <= limits.max_wheels,
        "wheel graph wheel limit exceeded"
    );
    let mut count = generated
        .len()
        .checked_add(transformed.len())
        .and_then(|count| count.checked_add(reserved.len()))
        .context("wheel graph input limit overflow")?;
    for manifest in manifests {
        count = count
            .checked_add(manifest.members().len())
            .and_then(|count| count.checked_add(2))
            .context("wheel graph source limit overflow")?;
        ensure!(
            count <= limits.max_sources,
            "wheel graph source limit exceeded"
        );
        for text in [manifest.name(), manifest.version(), manifest.dist_info()] {
            ensure!(
                !text.is_empty() && text.len() <= limits.max_path_bytes,
                "wheel graph distribution path limit exceeded"
            );
        }
        for member in manifest.members() {
            ensure!(
                member.archive_path().len() <= limits.max_path_bytes
                    && member.relative_path().len() <= limits.max_path_bytes,
                "wheel graph source path limit exceeded"
            );
        }
    }
    ensure!(
        count <= limits.max_sources,
        "wheel graph source limit exceeded"
    );
    Ok(())
}

fn absolute_path(path: &Path, label: &str) -> Result<PathBuf> {
    ensure!(
        path.is_absolute() && !path.as_os_str().as_encoded_bytes().contains(&0),
        "wheel {label} must be an absolute path"
    );
    let mut normalized = PathBuf::new();
    let mut depth = 0_usize;
    for part in path.components() {
        match part {
            Component::Normal(value) => {
                normalized.push(value);
                depth += 1;
            }
            Component::ParentDir => {
                ensure!(depth > 0, "wheel {label} path escapes its root");
                normalized.pop();
                depth -= 1;
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => normalized.push(part.as_os_str()),
        }
    }
    Ok(normalized)
}

fn relative_path(text: &str) -> Result<PathBuf> {
    ensure!(
        !text.is_empty()
            && !text.contains(['\\', '\0'])
            && !(text.as_bytes().get(1) == Some(&b':') && text.as_bytes()[0].is_ascii_alphabetic()),
        "wheel source has an invalid relative path"
    );
    let mut path = PathBuf::new();
    for part in Path::new(text).components() {
        match part {
            Component::Normal(value) => path.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(path.pop(), "wheel source relative path escapes its root")
            }
            _ => eyre::bail!("wheel source path must be relative"),
        }
    }
    ensure!(
        !path.as_os_str().is_empty(),
        "wheel source relative path is empty"
    );
    Ok(path)
}

fn check_path_limit(path: &Path, limits: WheelPlanLimits) -> Result<()> {
    ensure!(
        path.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
        "wheel graph path limit exceeded"
    );
    Ok(())
}

fn member_mode(member: &WheelMember) -> u32 {
    member.unix_mode().unwrap_or(0o644) & 0o777
}

struct ManifestState<'a> {
    manifest: &'a WheelManifest,
    installed_info: String,
    metadata_root: PathBuf,
    members: BTreeMap<usize, &'a WheelMember>,
}

impl ManifestState<'_> {
    fn metadata_path(&self, target: &WheelGeneratedTarget) -> PathBuf {
        self.metadata_root.join(match target {
            WheelGeneratedTarget::InstalledRecord => "RECORD",
            WheelGeneratedTarget::Installer => "INSTALLER",
            WheelGeneratedTarget::Scripts(_) => unreachable!("scripts do not have a metadata slot"),
        })
    }

    fn destination(&self, member: &WheelMember, scheme: &WheelInstallScheme) -> Result<PathBuf> {
        let mut relative = relative_path(member.relative_path())?;
        if matches!(member.scheme(), WheelScheme::Purelib | WheelScheme::Platlib) {
            let mut parts = relative.components();
            if parts
                .next()
                .is_some_and(|part| part.as_os_str() == self.manifest.dist_info())
            {
                relative = PathBuf::from(&self.installed_info).join(parts.as_path());
            }
        }
        Ok(scheme
            .root(member.scheme())
            .context("wheel source scheme is missing")?
            .join(relative))
    }
}

fn manifest_states<'a>(
    manifests: &'a [WheelManifest],
    scheme: &WheelInstallScheme,
    limits: WheelPlanLimits,
) -> Result<BTreeMap<WheelOwner, ManifestState<'a>>> {
    let mut states = BTreeMap::new();
    let mut names = BTreeSet::new();
    for manifest in manifests {
        ensure!(
            names.insert(manifest.name()),
            "wheel graph contains a duplicate distribution"
        );
        let owner = WheelOwner::from_manifest(manifest);
        let installed_info = format!(
            "{}-{}.dist-info",
            owner.name.replace('-', "_"),
            owner.version.replace('-', "_")
        );
        let metadata_root = scheme
            .root(manifest.root_scheme())
            .context("wheel root scheme is missing")?
            .join(&installed_info);
        check_path_limit(&metadata_root, limits)?;
        let mut members = BTreeMap::new();
        for member in manifest.members() {
            ensure!(
                members.insert(member.archive_index(), member).is_none(),
                "wheel graph contains duplicate source indices"
            );
        }
        states.insert(
            owner,
            ManifestState {
                manifest,
                installed_info,
                metadata_root,
                members,
            },
        );
    }
    Ok(states)
}

fn transform_map<'a>(
    intents: &'a [WheelTransformIntent],
    states: &BTreeMap<WheelOwner, ManifestState<'_>>,
) -> Result<BTreeMap<(WheelOwner, usize), &'a WheelTransformIntent>> {
    let mut result = BTreeMap::new();
    for intent in intents {
        let state = states
            .get(&intent.owner)
            .context("wheel transform has an unknown owner")?;
        let member = state
            .members
            .get(&intent.archive_index)
            .context("wheel transform references an unknown source")?;
        ensure!(
            member.scheme() == WheelScheme::Scripts && member.role() == WheelMemberRole::Payload,
            "wheel transform must refer to an archive script source"
        );
        ensure!(
            intent.mode <= 0o777,
            "wheel transform mode contains unsupported bits"
        );
        match intent.operation {
            WheelWriteOperation::Copy => ensure!(
                intent.sha256 == *member.observed_sha256()
                    && intent.size == member.size()
                    && intent.mode == member_mode(member),
                "wheel Copy transform disagrees with source content or mode"
            ),
            WheelWriteOperation::RewritePythonShebang => {}
            _ => eyre::bail!("wheel script transform has an invalid operation"),
        }
        ensure!(
            result
                .insert((intent.owner.clone(), intent.archive_index), intent)
                .is_none(),
            "wheel source has duplicate transform intents"
        );
    }
    Ok(result)
}

fn generated_map<'a>(
    intents: &'a [WheelGeneratedIntent],
    states: &BTreeMap<WheelOwner, ManifestState<'_>>,
    limits: WheelPlanLimits,
) -> Result<BTreeMap<(WheelOwner, WheelGeneratedTarget), &'a WheelGeneratedIntent>> {
    let mut result = BTreeMap::new();
    for intent in intents {
        ensure!(
            states.contains_key(&intent.owner),
            "wheel generated intent has an unknown owner"
        );
        ensure!(
            intent.mode <= 0o777,
            "wheel generated mode contains unsupported bits"
        );
        match (&intent.target, intent.operation) {
            (WheelGeneratedTarget::Scripts(name), WheelWriteOperation::GenerateEntryPoint) => {
                ensure!(
                    name.len() <= limits.max_path_bytes,
                    "wheel generated path limit exceeded"
                );
                let path = relative_path(name)?;
                ensure!(
                    path.components().count() == 1
                        && path.as_os_str() == name.as_str()
                        && !matches!(name.as_str(), "." | ".."),
                    "wheel generated script must have one safe basename"
                );
            }
            (WheelGeneratedTarget::InstalledRecord, WheelWriteOperation::GenerateRecord)
            | (WheelGeneratedTarget::Installer, WheelWriteOperation::GenerateInstaller) => {}
            _ => eyre::bail!("wheel generated operation does not match its fixed target"),
        }
        ensure!(
            result
                .insert((intent.owner.clone(), intent.target.clone()), intent)
                .is_none(),
            "wheel metadata or generated target has duplicate intents"
        );
    }
    Ok(result)
}

#[derive(Default)]
struct ReservedRule {
    file: bool,
    directory: bool,
    exclusive: bool,
}

struct Layout<'a, 'm> {
    scheme: &'a WheelInstallScheme,
    states: &'a BTreeMap<WheelOwner, ManifestState<'m>>,
    reserved: BTreeMap<PathBuf, ReservedRule>,
    metadata_owners: BTreeMap<PathBuf, WheelOwner>,
    limits: WheelPlanLimits,
    files: BTreeMap<PathBuf, WheelFilePlan>,
    directories: BTreeMap<PathBuf, BTreeSet<WheelOwner>>,
    pending: BTreeMap<PathBuf, WheelMetadataReservation>,
    total_bytes: u64,
}

impl<'a, 'm> Layout<'a, 'm> {
    fn new(
        scheme: &'a WheelInstallScheme,
        states: &'a BTreeMap<WheelOwner, ManifestState<'m>>,
        reserved: &[WheelReservedDestination],
        limits: WheelPlanLimits,
    ) -> Result<Self> {
        let mut reserved_index = BTreeMap::new();
        for entry in reserved {
            let rule: &mut ReservedRule = reserved_index.entry(entry.path.clone()).or_default();
            match entry.kind {
                WheelReservedKind::File => rule.file = true,
                WheelReservedKind::Directory => rule.directory = true,
                WheelReservedKind::ExclusiveTree => rule.exclusive = true,
            }
            ensure!(
                !(rule.file && rule.directory),
                "wheel reserved destination is both a file and directory"
            );
        }
        let mut metadata_owners = BTreeMap::new();
        for (owner, state) in states {
            ensure!(
                metadata_owners
                    .insert(state.metadata_root.clone(), owner.clone())
                    .is_none(),
                "wheel distributions have conflicting metadata roots"
            );
        }
        let mut layout = Self {
            scheme,
            states,
            reserved: reserved_index,
            metadata_owners,
            limits,
            files: BTreeMap::new(),
            directories: BTreeMap::new(),
            pending: BTreeMap::new(),
            total_bytes: 0,
        };
        // Scheme roots are required directories even if this graph does not
        // place a member in every scheme. They must never become payload files.
        for root in scheme.roots.values() {
            layout.add_directories(root, None)?;
        }
        for entry in reserved {
            ensure!(
                entry.kind != WheelReservedKind::File
                    || !layout.directories.contains_key(&entry.path),
                "wheel reserved file occupies a required scheme directory"
            );
        }
        Ok(layout)
    }

    fn add_directories(&mut self, path: &Path, owner: Option<&WheelOwner>) -> Result<()> {
        for directory in path.ancestors() {
            ensure!(
                directory.starts_with(self.scheme.final_prefix()),
                "wheel directory escapes final prefix"
            );
            check_path_limit(directory, self.limits)?;
            ensure!(
                !self.files.contains_key(directory) && !self.pending.contains_key(directory),
                "wheel file and directory destinations conflict"
            );
            if !self.directories.contains_key(directory) {
                ensure!(
                    self.directories.len() < self.limits.max_directories,
                    "wheel graph directory limit exceeded"
                );
            }
            let owners = self.directories.entry(directory.into()).or_default();
            if let Some(owner) = owner
                && !owners.insert(owner.clone())
            {
                break;
            }
            if directory == self.scheme.final_prefix() {
                break;
            }
        }
        Ok(())
    }

    fn check_target(&self, path: &Path, owner: &WheelOwner) -> Result<bool> {
        check_path_limit(path, self.limits)?;
        ensure!(
            path.starts_with(self.scheme.final_prefix()),
            "wheel destination escapes final prefix"
        );
        for ancestor in path.ancestors() {
            if let Some(rule) = self.reserved.get(ancestor) {
                ensure!(
                    !rule.file && !rule.exclusive && ancestor != path,
                    "wheel destination conflicts with a reserved runtime path"
                );
            }
            if ancestor == self.scheme.final_prefix() {
                break;
            }
        }
        if let Some((reserved, _)) = self
            .reserved
            .range::<Path, _>((Bound::Included(path), Bound::Unbounded))
            .next()
        {
            ensure!(
                !reserved.starts_with(path),
                "wheel file occupies an ancestor of a reserved runtime path"
            );
        }
        let state = self
            .states
            .get(owner)
            .context("wheel file has an unknown owner")?;
        let mut metadata = false;
        for ancestor in path.ancestors() {
            if let Some(other) = self.metadata_owners.get(ancestor) {
                ensure!(
                    owner == other,
                    "wheel metadata tree belongs to another owner"
                );
                metadata = true;
            }
            if ancestor == self.scheme.final_prefix() {
                break;
            }
        }
        for key in [WheelScheme::Purelib, WheelScheme::Platlib] {
            let root = self
                .scheme
                .root(key)
                .context("wheel library scheme is missing")?;
            if let Ok(relative) = path.strip_prefix(root)
                && let Some(Component::Normal(first)) = relative.components().next()
                && first
                    .to_str()
                    .is_some_and(|first| first.ends_with(".dist-info"))
            {
                ensure!(
                    first == state.installed_info.as_str()
                        && root.join(first) == state.metadata_root,
                    "wheel destination injects foreign or duplicate metadata"
                );
                metadata = true;
            }
        }
        Ok(metadata)
    }

    fn reserve_metadata(&mut self, owner: &WheelOwner, path: PathBuf) -> Result<()> {
        self.check_target(&path, owner)?;
        ensure!(
            !self.directories.contains_key(&path)
                && !self.files.contains_key(&path)
                && !self.pending.contains_key(&path),
            "wheel metadata reservation conflicts with a destination"
        );
        self.check_file_count()?;
        let target = if path.file_name().is_some_and(|name| name == "RECORD") {
            WheelGeneratedTarget::InstalledRecord
        } else {
            WheelGeneratedTarget::Installer
        };
        self.add_directories(
            path.parent()
                .context("wheel metadata destination has no parent")?,
            Some(owner),
        )?;
        self.pending.insert(
            path.clone(),
            WheelMetadataReservation {
                owner: owner.clone(),
                target,
                destination: path,
            },
        );
        Ok(())
    }

    fn check_file_count(&self) -> Result<()> {
        let count = self
            .files
            .len()
            .checked_add(self.pending.len())
            .context("wheel graph file limit overflow")?;
        ensure!(
            count < self.limits.max_files,
            "wheel graph file limit exceeded"
        );
        Ok(())
    }

    fn insert_file(
        &mut self,
        mut file: WheelFilePlan,
        slot: Option<(WheelOwner, WheelGeneratedTarget)>,
    ) -> Result<()> {
        let owner = file
            .owners
            .first()
            .context("wheel file has no owner")?
            .clone();
        let metadata = self.check_target(&file.destination, &owner)?;
        ensure!(
            !self.directories.contains_key(&file.destination),
            "wheel file and directory destinations conflict"
        );
        if let Some(reservation) = self.pending.get(&file.destination) {
            ensure!(
                slot.as_ref()
                    .is_some_and(|(owner, target)| owner == &reservation.owner
                        && target == &reservation.target),
                "wheel metadata slot requires its owner's generated intent"
            );
        } else {
            ensure!(
                slot.is_none(),
                "wheel metadata replacement has no pending owner slot"
            );
        }
        if let Some(existing) = self.files.get_mut(&file.destination) {
            ensure!(!metadata, "wheel metadata destination has multiple sources");
            ensure!(
                existing.sha256 == file.sha256 && existing.size == file.size,
                "wheel file content conflict"
            );
            ensure!(existing.mode == file.mode, "wheel file mode conflict");
            ensure!(
                existing.operation == file.operation,
                "wheel file operation conflict"
            );
            existing.owners.extend(file.owners);
            existing.sources.extend(file.sources);
            existing.owners.sort();
            existing.owners.dedup();
            existing.sources.sort();
            existing.sources.dedup();
            let parent = existing.destination.parent().unwrap().to_path_buf();
            self.add_directories(&parent, Some(&owner))?;
            return Ok(());
        }
        if !self.pending.contains_key(&file.destination) {
            self.check_file_count()?;
        }
        let total = self
            .total_bytes
            .checked_add(file.size)
            .context("wheel graph byte limit overflow")?;
        ensure!(
            total <= self.limits.max_total_bytes,
            "wheel graph byte limit exceeded"
        );
        self.add_directories(
            file.destination
                .parent()
                .context("wheel destination has no parent")?,
            Some(&owner),
        )?;
        self.pending.remove(&file.destination);
        file.sources.sort();
        file.sources.dedup();
        self.total_bytes = total;
        self.files.insert(file.destination.clone(), file);
        Ok(())
    }

    fn finish(self) -> WheelGraphPlan {
        WheelGraphPlan {
            final_prefix: self.scheme.final_prefix.clone(),
            files: self.files.into_values().collect(),
            directories: self
                .directories
                .into_iter()
                .filter(|(_, owners)| !owners.is_empty())
                .map(|(destination, owners)| WheelDirectoryPlan {
                    destination,
                    owners: owners.into_iter().collect(),
                })
                .collect(),
            pending_metadata: self.pending.into_values().collect(),
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
