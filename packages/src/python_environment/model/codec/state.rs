//! Revalidate current facts and revision history without filesystem authority.

use super::*;

pub(super) fn validate(model: &CatalogModel, limits: ModelLimits) -> Result<()> {
    limits.validate()?;
    ensure!(
        model.generations.len() <= limits.max_generation_records,
        "catalog generation record limit exceeded"
    );
    ensure!(
        model.selections.len() <= limits.max_selection_slots,
        "catalog selection slot limit exceeded"
    );
    ensure!(
        model.transactions.len() <= limits.max_transactions,
        "catalog transaction limit exceeded"
    );
    let mut references = 0_usize;
    let mut shared_count = 0_usize;
    for (allocation, record) in &model.generations {
        record.validate(limits)?;
        if let Some(layout) = record.shared_layout() {
            ensure!(
                model.shared_layouts.get(layout) == Some(allocation),
                "catalog shared layout identity has inconsistent or multiple allocations"
            );
            shared_count += 1;
        }
        ensure!(
            allocation == record.allocation(),
            "catalog generation key disagrees with record"
        );
        references = references
            .checked_add(record.inputs.len())
            .ok_or_else(|| eyre::eyre!("catalog reference limit overflow"))?;
        ensure!(
            references <= limits.max_total_references,
            "catalog total reference limit exceeded"
        );
        ensure!(
            record.inputs.windows(2).all(|pair| pair[0] < pair[1]),
            "catalog input references are not canonical"
        );
    }
    ensure!(
        shared_count == model.shared_layouts.len(),
        "catalog shared layout index is inconsistent"
    );
    ensure!(
        references == model.total_references,
        "catalog total reference count is inconsistent"
    );
    for (key, stamp) in &model.selections {
        key.validate(limits)?;
        validate_stamp(stamp)?;
        ensure!(
            stamp.revision != 0,
            "catalog cannot store a default selection slot"
        );
        if let Some(allocation) = &stamp.generation {
            let record = model
                .generations
                .get(allocation)
                .ok_or_else(|| eyre::eyre!("catalog active generation is missing"))?;
            ensure!(
                record.target() == key.target(),
                "catalog active generation has an incompatible target"
            );
        }
    }
    ensure!(
        model.projects.len() <= limits.max_selection_slots,
        "catalog project registration limit exceeded"
    );
    for (project, fact) in &model.projects {
        ensure!(!project.is_nil(), "catalog project identity is nil");
        ensure!(
            ProjectRegistration::from_facts(fact.root(), fact.declaration()).is_some(),
            "invalid catalog project registration"
        );
    }
    let mut histories = BTreeMap::<&SelectionKey, BTreeMap<u64, (&Uuid, &AppliedChange)>>::new();
    for (id, change) in &model.transactions {
        ensure!(!id.is_nil(), "catalog transaction identity is nil");
        change.key.validate(limits)?;
        validate_stamp(&change.expected)?;
        if let Some(allocation) = &change.generation {
            GenerationRecord::validate_allocation(allocation)?;
            if let Some(record) = model.generations.get(allocation) {
                ensure!(
                    record.target() == change.key.target(),
                    "catalog historical generation has an incompatible target"
                );
            }
        }
        ensure!(
            histories
                .entry(&change.key)
                .or_default()
                .insert(change.expected.revision, (id, change))
                .is_none(),
            "catalog transaction history forks at a revision"
        );
    }
    ensure!(
        histories.len() == model.selections.len(),
        "catalog selections and transaction histories differ"
    );
    for (key, changes) in histories {
        let mut stamp = SelectionStamp::default();
        for (id, change) in changes.into_values() {
            ensure!(
                change.expected == stamp,
                "catalog transaction expected stamp breaks its history chain"
            );
            let revision = stamp
                .revision
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("catalog selection revision overflow"))?;
            stamp = SelectionStamp {
                revision,
                generation: change.generation.clone(),
                language: change.language.clone(),
                transaction: *id,
            };
        }
        ensure!(
            model.selections.get(key) == Some(&stamp),
            "catalog final selection disagrees with history"
        );
    }
    Ok(())
}

pub(super) fn validate_stamp(stamp: &SelectionStamp) -> Result<()> {
    if let Some(allocation) = &stamp.generation {
        GenerationRecord::validate_allocation(allocation)?;
    }
    if let Some(language) = &stamp.language {
        ensure!(
            stamp.generation.is_none(),
            "catalog stamp cannot combine a generation with a language identity"
        );
        let digest: Digest = language
            .parse()
            .map_err(|_| eyre::eyre!("invalid catalog language identity"))?;
        ensure!(
            matches!(digest, Digest::Sha256(_)) && digest.to_string() == *language,
            "catalog language identity is not canonical"
        );
    }
    if stamp.revision == 0 {
        ensure!(
            stamp.generation.is_none() && stamp.language.is_none() && stamp.transaction.is_nil(),
            "catalog default stamp has active facts"
        );
    } else {
        ensure!(
            !stamp.transaction.is_nil(),
            "catalog nondefault stamp is missing its transaction"
        );
    }
    Ok(())
}
