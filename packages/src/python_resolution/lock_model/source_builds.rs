//! Exact build receipts extend the portable graph; they never grant IO authority.
use super::*;
use crate::packages::python_environment::model::SourceBuildRecord;

impl PythonLockModel {
    pub(crate) fn builds(&self) -> &[(PinId, SourceBuildRecord)] {
        &self.builds
    }
    pub(crate) fn source_build(&self, pin: PinId) -> Option<&SourceBuildRecord> {
        self.builds
            .iter()
            .find(|(id, _)| *id == pin)
            .map(|(_, record)| record)
    }
    pub(crate) fn from_parts_with_builds(
        pins: &[ContentPin],
        graphs: &[LockGraphInput<'_>],
        builds: &[(PinId, SourceBuildRecord)],
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        limits.validate()?;
        count(pins.len(), limits.max_pins, LockLimit::Pins)?;
        count(builds.len(), limits.max_pins, LockLimit::Pins)?;
        let mut supplied = BTreeSet::new();
        for pin in pins {
            if !supplied.insert(pin.id()) {
                return Err(LockModelError::Duplicate(LockIdentity::Pin));
            }
        }
        // The ordinary graph constructor retains its original unused-pin rule.
        // Build inputs are added only from validated, actually referenced receipts.
        let used: BTreeSet<_> = graphs
            .iter()
            .flat_map(|g| g.nodes.iter().map(|n| n.source_pin))
            .collect();
        let main: Vec<_> = pins
            .iter()
            .filter(|p| used.contains(&p.id()))
            .cloned()
            .collect();
        let mut model = Self::from_parts(&main, graphs, limits)?;
        let mut all: BTreeMap<_, _> = pins.iter().map(|p| (p.id(), p.clone())).collect();
        for (_, record) in builds {
            record
                .validate()
                .map_err(|_| LockModelError::IdentityMismatch)?;
            for input in record
                .inputs
                .iter()
                .chain(record.observation.iter().flat_map(|o| &o.initial_inputs))
            {
                let pin = input
                    .content_pin()
                    .map_err(|_| LockModelError::IdentityMismatch)?;
                if all.get(&pin.id()).is_some_and(|old| old != &pin) {
                    return Err(LockModelError::IdentityMismatch);
                }
                all.insert(pin.id(), pin);
                count(all.len(), limits.max_pins, LockLimit::Pins)?;
            }
        }
        model.builds = builds.to_vec();
        model.pins = all.into_values().collect();
        close_owned(model, limits)
    }
}

pub(super) fn validate(
    model: &PythonLockModel,
    used: &mut BTreeSet<PinId>,
    usage: &mut Usage,
    limits: LockModelLimits,
) -> LockModelResult<()> {
    count(model.builds.len(), limits.max_pins, LockLimit::Pins)?;
    let pins: BTreeMap<_, _> = model.pins.iter().map(|p| (p.id(), p)).collect();
    let mut records = BTreeMap::new();
    for (id, record) in &model.builds {
        if records.insert(*id, record).is_some() {
            return Err(LockModelError::Duplicate(LockIdentity::Pin));
        }
        record
            .validate()
            .map_err(|_| LockModelError::IdentityMismatch)?;
        if let Some(native) = &record.native {
            let raw = serde_json::to_string(native).map_err(|_| LockModelError::InvalidField(LockField::Native))?;
            let maximum = crate::packages::python_environment::model::MAX_NATIVE_JSON_BYTES.min(limits.max_declaration_bytes);
            count(raw.len(), maximum, LockLimit::FieldBytes)?;
            charge(&mut usage.utf8, raw.len(), limits.max_total_utf8_bytes, LockLimit::TotalUtf8Bytes)?;
        }
        if record.observation.is_none() {
            return Err(LockModelError::UnsupportedSourceKind);
        }
        let pin = pins
            .get(id)
            .ok_or(LockModelError::Reference(LockReference::NodePin))?;
        if pin.file().kind() != RegistryArtifactKind::Sdist
            || pin.digest().to_string() != record.source_digest
        {
            return Err(LockModelError::IdentityMismatch);
        }
        let wheel = pakro_index::parse_wheel_filename(&record.output_filename)
            .ok_or(LockModelError::IdentityMismatch)?;
        if pakro_index::normalize_py_name(&wheel.name) != pin.file().project()
            || !same_version(&wheel.version, pin.file().version())
        {
            return Err(LockModelError::IdentityMismatch);
        }
        for value in [
            &record.source_digest,
            &record.backend,
            &record.output_filename,
            &record.output_digest,
        ] {
            text(value, LockField::Filename, false, usage, limits)?;
        }
        let observation = record
            .observation
            .as_ref()
            .ok_or(LockModelError::UnsupportedSourceKind)?;
        for value in observation
            .initial_requirements
            .iter()
            .chain(&observation.dynamic_requirements)
        {
            text(value, LockField::Declaration, true, usage, limits)?;
        }
        charge(
            &mut usage.references,
            record.inputs.len() + observation.initial_inputs.len(),
            limits.max_total_references,
            LockLimit::References,
        )?;
        for input in record
            .inputs
            .iter()
            .chain(record.observation.iter().flat_map(|o| &o.initial_inputs))
        {
            for value in [
                &input.name,
                &input.version,
                &input.source_pin,
                &input.registry,
                &input.source_filename,
                &input.source_digest,
                &input.output_filename,
                &input.output_digest,
            ] {
                text(value, LockField::Filename, false, usage, limits)?;
            }
        }
    }
    let mut pending: Vec<_> = used.iter().copied().collect();
    let mut reached = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !reached.insert(id) {
            continue;
        }
        if (model.declaration_fingerprint.is_some() || !model.builds.is_empty())
            && pins
                .get(&id)
                .is_some_and(|pin| pin.file().kind() == RegistryArtifactKind::Sdist)
            && !records.contains_key(&id)
        {
            return Err(LockModelError::Reference(LockReference::NodePin));
        }
        let Some(record) = records.get(&id) else {
            continue;
        };
        for input in record
            .inputs
            .iter()
            .chain(record.observation.iter().flat_map(|o| &o.initial_inputs))
        {
            let pin = input
                .content_pin()
                .map_err(|_| LockModelError::IdentityMismatch)?;
            if pins.get(&pin.id()).is_none_or(|actual| **actual != pin) {
                return Err(LockModelError::IdentityMismatch);
            }
            if input.source_is_sdist {
                let child = records
                    .get(&pin.id())
                    .ok_or(LockModelError::Reference(LockReference::NodePin))?;
                if child.output_filename != input.output_filename
                    || child.output_digest != input.output_digest
                    || child.output_size != input.output_size
                {
                    return Err(LockModelError::IdentityMismatch);
                }
            }
            used.insert(pin.id());
            pending.push(pin.id());
        }
    }
    if records.keys().any(|id| !reached.contains(id)) {
        return Err(LockModelError::Reference(LockReference::UnusedPin));
    }
    // Build inputs form a finite preparation order. Reject a locked source
    // cycle before any fetching or worker execution, without recursive walks.
    let mut remaining = BTreeMap::new();
    let mut parents: BTreeMap<PinId, Vec<PinId>> = BTreeMap::new();
    for (id, record) in &records {
        let mut children = BTreeSet::new();
        for input in record
            .inputs
            .iter()
            .chain(record.observation.iter().flat_map(|o| &o.initial_inputs))
        {
            if input.source_is_sdist {
                let child = PinId::parse_hex(&input.source_pin)?;
                if children.insert(child) {
                    parents.entry(child).or_default().push(*id);
                }
            }
        }
        remaining.insert(*id, children.len());
    }
    let mut ready: Vec<_> = remaining
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut completed = 0;
    while let Some(id) = ready.pop() {
        completed += 1;
        for parent in parents.get(&id).into_iter().flatten() {
            let count = remaining
                .get_mut(parent)
                .ok_or(LockModelError::IdentityMismatch)?;
            *count = count
                .checked_sub(1)
                .ok_or(LockModelError::IdentityMismatch)?;
            if *count == 0 {
                ready.push(*parent);
            }
        }
    }
    if completed != records.len() {
        return Err(LockModelError::Reference(LockReference::UnreachableNode));
    }
    Ok(())
}
