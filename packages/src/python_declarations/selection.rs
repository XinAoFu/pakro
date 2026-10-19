//! Lazy group semantics and explicitly selected, unfiltered declaration data.
use super::*;
use std::collections::BTreeSet;
type E = PythonDeclarationError;
type L = PythonDeclarationLimit;

struct Budget {
    limits: PythonExpansionLimits,
    work: usize,
    utf8: usize,
    count: usize,
}
impl Budget {
    fn work(&mut self) -> PythonDeclarationResult<()> {
        self.work = self
            .work
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_work)
            .ok_or(E::Limit(L::ExpansionWork))?;
        Ok(())
    }
    fn text(&mut self, value: &str) -> PythonDeclarationResult<()> {
        self.utf8 = self
            .utf8
            .checked_add(value.len())
            .filter(|n| *n <= self.limits.max_total_utf8_bytes)
            .ok_or(E::Limit(L::Utf8Bytes))?;
        Ok(())
    }
    fn append(
        &mut self,
        output: &mut Vec<PythonDeclaredRequirement>,
        syntax: Arc<PythonRequirementSyntax>,
        origin: PythonDeclarationOrigin,
    ) -> PythonDeclarationResult<()> {
        self.work()?;
        self.count = self
            .count
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_expanded_requirements)
            .ok_or(E::Limit(L::ExpandedRequirements))?;
        self.text(syntax.raw_for_semantic_resolution())?;
        if let PythonDeclarationOrigin::Extra { name, .. }
        | PythonDeclarationOrigin::Group { name, .. } = &origin
        {
            self.text(name)?;
        }
        output
            .try_reserve(1)
            .map_err(|_| E::Limit(L::ExpandedRequirements))?;
        output.push(PythonDeclaredRequirement { syntax, origin });
        Ok(())
    }
}
fn names(
    raw: &[&str],
    maximum: usize,
    field: PythonDeclarationField,
    budget: &mut Budget,
) -> PythonDeclarationResult<Vec<String>> {
    if raw.len() > maximum {
        return Err(E::Limit(match field {
            PythonDeclarationField::OptionalDependencies => L::Extras,
            _ => L::Groups,
        }));
    }
    let mut out = Vec::new();
    let mut used = BTreeSet::new();
    out.try_reserve_exact(raw.len())
        .map_err(|_| E::Limit(L::Values))?;
    for value in raw {
        budget.work()?;
        budget.text(value)?;
        let name = NormalizedPythonName::new(value).map_err(|_| E::InvalidField(field))?;
        if !used.insert(name.clone()) {
            return Err(E::DuplicateSelection);
        }
        budget.text(name.as_str())?;
        out.push(name.as_str().to_owned());
    }
    Ok(out)
}
struct Frame<'a> {
    name: &'a str,
    index: usize,
}
fn expand<'a>(
    project: &'a ParsedPythonProject,
    name: &str,
    output: &mut Vec<PythonDeclaredRequirement>,
    budget: &mut Budget,
    memo: &mut BTreeMap<(&'a str, usize), Arc<PythonRequirementSyntax>>,
) -> PythonDeclarationResult<()> {
    let (name, _) = project
        .groups
        .get_key_value(name)
        .ok_or(E::MissingSelection(PythonDeclarationNamespace::Group))?;
    let mut stack = vec![Frame { name, index: 0 }];
    let mut active = BTreeSet::from([name.as_str()]);
    while let Some(frame) = stack.last() {
        budget.work()?;
        if stack.len() > budget.limits.max_depth {
            return Err(E::Limit(L::Depth));
        }
        let (name, index) = (frame.name, frame.index);
        let group = project
            .groups
            .get(name)
            .ok_or(E::MissingSelection(PythonDeclarationNamespace::Group))?;
        let RawGroup::Items(items) = &group.contents else {
            return Err(E::InvalidGroupItem);
        };
        if index == items.len() {
            stack.pop();
            active.remove(name);
            continue;
        }
        stack.last_mut().expect("nonempty expansion stack").index += 1;
        match &items[index] {
            RawGroupItem::Requirement(raw) => {
                let syntax = match memo.get(&(name, index)) {
                    Some(value) => Arc::clone(value),
                    None => {
                        let value = decode::requirement(raw, project.requirement_limits)?;
                        memo.insert((name, index), Arc::clone(&value));
                        value
                    }
                };
                budget.append(
                    output,
                    syntax,
                    PythonDeclarationOrigin::Group {
                        name: name.to_owned(),
                        index,
                    },
                )?;
            }
            RawGroupItem::Include(raw) => {
                let normalized = NormalizedPythonName::new(raw).map_err(|_| E::InvalidGroupItem)?;
                let (name, _) = project
                    .groups
                    .get_key_value(normalized.as_str())
                    .ok_or(E::MissingSelection(PythonDeclarationNamespace::Group))?;
                if !active.insert(name.as_str()) {
                    return Err(E::GroupCycle);
                }
                if stack.len() >= budget.limits.max_depth {
                    return Err(E::Limit(L::Depth));
                }
                stack.try_reserve(1).map_err(|_| E::Limit(L::Depth))?;
                stack.push(Frame { name, index: 0 });
            }
            RawGroupItem::Invalid => return Err(E::InvalidGroupItem),
        }
    }
    Ok(())
}
pub(super) fn select(
    project: &ParsedPythonProject,
    selection: PythonProjectSelection<'_>,
    limits: PythonExpansionLimits,
) -> PythonDeclarationResult<PythonDeclarationSelection> {
    if [
        limits.max_depth,
        limits.max_work,
        limits.max_expanded_requirements,
        limits.max_total_utf8_bytes,
        limits.max_selected_extras,
        limits.max_selected_groups,
    ]
    .contains(&0)
    {
        return Err(E::InvalidLimits);
    }
    let mut budget = Budget {
        limits,
        work: 0,
        utf8: 0,
        count: 0,
    };
    let extras = names(
        selection.extras,
        limits.max_selected_extras,
        PythonDeclarationField::OptionalDependencies,
        &mut budget,
    )?;
    let groups = names(
        selection.groups,
        limits.max_selected_groups,
        PythonDeclarationField::DependencyGroups,
        &mut budget,
    )?;
    let wants_project = selection.include_project_dependencies || !extras.is_empty();
    let mut requirements = Vec::new();
    if selection.include_project_dependencies {
        for (index, syntax) in project.dependencies.iter().enumerate() {
            budget.append(
                &mut requirements,
                Arc::clone(syntax),
                PythonDeclarationOrigin::Project { index },
            )?;
        }
    }
    for name in &extras {
        budget.work()?;
        match project.extras.get(name) {
            Some(extra) => {
                for (index, syntax) in extra.requirements.iter().enumerate() {
                    budget.append(
                        &mut requirements,
                        Arc::clone(syntax),
                        PythonDeclarationOrigin::Extra {
                            name: name.clone(),
                            index,
                        },
                    )?;
                }
            }
            None if project.project.is_none()
                || project
                    .dynamic
                    .iter()
                    .any(|field| field == "optional-dependencies") => {}
            None => return Err(E::MissingSelection(PythonDeclarationNamespace::Extra)),
        }
    }
    let mut memo = BTreeMap::new();
    for name in &groups {
        expand(project, name, &mut requirements, &mut budget, &mut memo)?;
    }
    let known = SelectedPythonDeclarations {
        requirements,
        extras,
        groups,
    };
    if wants_project && project.project.is_none() {
        return Ok(PythonDeclarationSelection::NeedsMetadataWorker {
            known,
            need: PythonMetadataWorkerNeed {
                reason: PythonMetadataWorkerReason::MissingProject,
                fields: Vec::new(),
            },
        });
    }
    if wants_project && !project.dynamic.is_empty() {
        for field in &project.dynamic {
            budget.text(field)?;
        }
        return Ok(PythonDeclarationSelection::NeedsMetadataWorker {
            known,
            need: PythonMetadataWorkerNeed {
                reason: PythonMetadataWorkerReason::DynamicMetadata,
                fields: project.dynamic.clone(),
            },
        });
    }
    Ok(PythonDeclarationSelection::Static(known))
}
