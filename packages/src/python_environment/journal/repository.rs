//! Same-session bounded metadata operations. No prefix cleanup or publication.

use super::*;
use crate::packages::python_environment::prefix::control::ControlNamespace;

pub(super) struct Batch {
    pub records: Vec<JournalRecord>,
    document_bytes: usize,
    name_bytes: usize,
    raw_inputs: usize,
}

pub(super) fn load(control: &StoreControl<'_>, limits: JournalLimits) -> Result<Batch> {
    limits.validate()?;
    let (dev, ino) = control.store_identity()?;
    let store = [dev, ino];
    let names = control.list_names(
        ControlNamespace::Journals,
        limits.max_documents,
        limits.max_total_name_bytes,
    )?;
    let mut batch = Batch {
        records: Vec::new(),
        document_bytes: 0,
        name_bytes: 0,
        raw_inputs: 0,
    };
    for name in names {
        validate_name(&name)?;
        let remaining = limits
            .max_total_document_bytes
            .checked_sub(batch.document_bytes)
            .filter(|remaining| *remaining != 0)
            .ok_or_else(|| eyre::eyre!("journal total document byte limit exceeded"))?;
        let maximum = u64::try_from(limits.max_document_bytes.min(remaining))
            .map_err(|_| eyre::eyre!("journal document byte limit is not representable"))?;
        let snapshot = control
            .read_bounded(ControlNamespace::Journals, &name, maximum)?
            .ok_or_else(|| eyre::eyre!("journal entry disappeared during bounded read"))?;
        batch.document_bytes = charge(
            batch.document_bytes,
            snapshot.bytes().len(),
            limits.max_total_document_bytes,
            "journal total document byte limit exceeded",
        )?;
        batch.name_bytes = charge(
            batch.name_bytes,
            name.len(),
            limits.max_total_name_bytes,
            "journal total name byte limit exceeded",
        )?;
        let record = read::decode(
            snapshot.bytes(),
            &name,
            store,
            limits,
            &mut batch.raw_inputs,
        )?;
        ensure!(
            batch.records.len() < limits.max_documents,
            "journal document count limit exceeded"
        );
        batch
            .records
            .try_reserve(1)
            .map_err(|_| eyre::eyre!("journal collection allocation failed"))?;
        batch.records.push(record);
    }
    // list_names is sorted by canonical nonce filename; retain explicit wire
    // ordering without depending on the filesystem's directory iteration order.
    batch.records.sort_unstable_by_key(|record| record.nonce);
    Ok(batch)
}

pub(super) fn prepare(
    control: &mut StoreControl<'_>,
    request: &JournalPrepare<'_>,
    limits: JournalLimits,
) -> Result<JournalRecord> {
    limits.validate()?;
    ensure!(!request.nonce.is_nil(), "journal nonce must not be nil");
    let key = SelectionKey::new(
        request.key.project(),
        request.key.group(),
        *request.key.target(),
        limits.model,
    )?;
    let expected = JournalExpectedStamp {
        revision: request.expected.revision(),
        generation: request.expected.generation().map(str::to_owned),
        transaction: request.expected.transaction(),
    };
    validate_expected(&expected)?;
    let (dev, ino) = control.store_identity()?;
    let store = [dev, ino];
    let (target, raw_inputs) = match &request.target {
        JournalTarget::Generation {
            identity,
            graph,
            inputs,
        } => {
            ensure!(
                inputs.len() <= limits.max_inputs_per_record
                    && inputs.len() <= limits.max_total_inputs,
                "journal input reference limit exceeded"
            );
            for input in *inputs {
                input.validate()?;
            }
            let facts = identity.facts()?;
            ensure!(
                facts.store == store,
                "journal generation belongs to a different store"
            );
            validate_allocation(&facts.allocation)?;
            let mut canonical = Vec::new();
            canonical
                .try_reserve_exact(inputs.len())
                .map_err(|_| eyre::eyre!("journal input allocation failed"))?;
            canonical.extend_from_slice(inputs);
            canonical.sort();
            canonical.dedup();
            (
                RecordedTarget::Generation(JournalGenerationFacts {
                    identity: facts,
                    graph: *graph,
                    inputs: canonical,
                }),
                inputs.len(),
            )
        }
        JournalTarget::ClearSelection => (RecordedTarget::ClearSelection { store }, 0),
    };
    let record = JournalRecord {
        nonce: request.nonce,
        key,
        expected,
        target,
    };
    let batch = load(control, limits)?;
    if let Some(existing) = batch
        .records
        .iter()
        .find(|existing| existing.nonce == record.nonce)
    {
        ensure!(
            existing == &record,
            "journal nonce already records different immutable facts"
        );
        // A retry validates the full batch but does not rewrite its inode/bytes.
        return Ok(record);
    }
    let bytes = encode::encode(&record, limits.max_document_bytes)?;
    let name = filename(record.nonce);
    charge(
        batch.records.len(),
        1,
        limits.max_documents,
        "journal document count limit exceeded",
    )?;
    charge(
        batch.document_bytes,
        bytes.len(),
        limits.max_total_document_bytes,
        "journal total document byte limit exceeded",
    )?;
    charge(
        batch.name_bytes,
        name.len(),
        limits.max_total_name_bytes,
        "journal total name byte limit exceeded",
    )?;
    charge(
        batch.raw_inputs,
        raw_inputs,
        limits.max_total_inputs,
        "journal total input reference limit exceeded",
    )?;
    if let JournalTarget::Generation { identity, .. } = &request.target {
        ensure!(
            record.generation().is_some_and(|facts| identity
                .facts()
                .is_ok_and(|current| current == facts.identity)),
            "journal generation identity changed before persistence"
        );
    }
    control.replace(ControlNamespace::Journals, &name, None, &bytes)?;
    Ok(record)
}

fn charge(current: usize, amount: usize, maximum: usize, label: &'static str) -> Result<usize> {
    current
        .checked_add(amount)
        .filter(|total| *total <= maximum)
        .ok_or_else(|| eyre::eyre!(label))
}

fn validate_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    ensure!(
        bytes.len() == 37
            && bytes.ends_with(b".json")
            && bytes[..32]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)),
        "unknown journal entry name"
    );
    Ok(())
}
