//! Language and environment facts from one observation and deadline. This
//! module only describes existing objects; all guards end before application.
use super::{
    paths::selection_paths,
    snapshot::{LanguageCandidate, SelectionReport, open_query_context, select_target},
};
use crate::libraries::project::{
    identity::ProjectIdentity, package_graph::CommittedGraph, repository::ProjectRepository,
    snapshot::ObservedProjectInputs,
};
use crate::programs::shared::python_roots;
use crate::programs::{
    OperationContext,
    directory::{DirectoryPython, PythonQueryReport, PythonQueryRequest, PythonQueryScope},
};
use eyre::{ContextCompat, Result, ensure};

#[derive(Debug)]
pub(crate) struct ObservedSelection {
    pub(crate) report: PythonQueryReport,
    pub(crate) candidate: Option<LanguageCandidate>,
    pub(crate) graph: Option<CommittedGraph>,
    pub(crate) already_applied: bool,
}
impl ObservedSelection {
    fn report(report: PythonQueryReport) -> Self {
        Self {
            report,
            candidate: None,
            graph: None,
            already_applied: false,
        }
    }
}

pub(crate) fn query_observed(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    request: &PythonQueryRequest,
) -> Result<ObservedSelection> {
    let result = query_observed_inner(context, observed, request)?;
    context.checkpoint()?;
    observed.recheck()?;
    crate::programs::shared::lock_transaction::ensure_project_consistent(context, observed)?;
    Ok(result)
}

fn query_observed_inner(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    request: &PythonQueryRequest,
) -> Result<ObservedSelection> {
    let query = open_query_context(context, observed)?;
    let candidate = match select_target(&query, request.target.as_deref())? {
        SelectionReport::Selected(candidate) => candidate,
        SelectionReport::NoLanguageRequest => {
            return Ok(ObservedSelection::report(PythonQueryReport::NoDeclaration));
        }
        SelectionReport::InvalidDeclaration { requested } => {
            return Ok(ObservedSelection::report(
                PythonQueryReport::InvalidDeclaration { requested },
            ));
        }
        SelectionReport::Missing { requested } => {
            return Ok(ObservedSelection::report(
                PythonQueryReport::MissingLanguage { requested },
            ));
        }
        SelectionReport::Ambiguous {
            requested,
            candidates,
        } => {
            return Ok(ObservedSelection::report(PythonQueryReport::Ambiguous {
                requested,
                candidates: candidates
                    .iter()
                    .map(|candidate| candidate.key().filename())
                    .collect(),
            }));
        }
        SelectionReport::Corrupt { detail } => {
            return Ok(ObservedSelection::report(PythonQueryReport::Corrupt {
                detail,
            }));
        }
    };
    let languages =
        crate::libraries::language::repository::LanguageRepository::open_readonly(context)?;
    let language = languages
        .acquire_by_key_for_query(candidate.key())?
        .context("selected Python is no longer Ready")?;
    language.check_metadata()?;
    let paths = selection_paths(&candidate);
    let mut selected = DirectoryPython {
        root: paths.root().to_owned(),
        instance: candidate.key().filename(),
        bin_dir: paths.bin_dir().to_owned(),
        bin_dirs: vec![paths.bin_dir().to_owned()],
        executable: paths.executable().to_owned(),
        version: candidate.version().to_owned(),
        activation: None,
    };
    if request.scope == PythonQueryScope::Language {
        observed.recheck()?;
        context.checkpoint()?;
        return Ok(ObservedSelection {
            report: PythonQueryReport::Ready(selected),
            candidate: Some(candidate),
            graph: None,
            already_applied: false,
        });
    }
    if language.spec().plan().conda_record().is_some() {
        return query_conda_environment(context, observed, candidate, selected, &language);
    }
    // Full selection validates complete Pakro intent; marker activation uses
    // the actual selected interpreter while inactive declarations stay locked.
    let complete_roots =
        match crate::programs::python::project_roots(observed, candidate.exact_version(), &[]) {
            Ok(roots) => roots,
            Err(error) if request.target.is_some() => {
                return Ok(ObservedSelection {
                    report: PythonQueryReport::IncompatibleTarget {
                        requested: candidate.exact_version().to_owned(),
                        detail: format!("{error:#}"),
                    },
                    candidate: Some(candidate),
                    graph: None,
                    already_applied: false,
                });
            }
            Err(error) => return Err(error),
        };
    let locked_fingerprint = if let Some(model) = super::snapshot::project_lock_for_selected_query(
        context,
        observed,
        request.target.as_deref(),
        &language,
    )? {
        let locked = crate::programs::install::lock::validate_project_lock_model_roots(
            &model,
            &language,
            &complete_roots,
        )?;
        Some(crate::programs::install::lock::portable_graph(&model, locked)?.fingerprint()?)
    } else {
        None
    };
    let roots = python_roots::active_roots(&complete_roots, &language)?;
    let mut repository = ProjectRepository::open_readonly(context)?;
    let identity = ProjectIdentity::from_observation(observed)?;
    let declaration = crate::programs::shared::commit::declaration_digest(observed);
    let mut chosen_graph = None;
    if !roots.is_empty() {
        let sources = context.python_sources()?;
        ensure!(
            sources.registries.len() == 1,
            "shared package selection requires one fixed registry source"
        );
        let allowed: Vec<_> = sources.registries.iter().map(|registry| (
            registry.id().as_str(), crate::packages::python_resolution::lock_model::canonical_registry_base_fingerprint(registry.canonical_base())
        )).collect();
        let requested = python_roots::pairs(&roots);
        let graphs = repository
            .read_views()
            .matching_package_graphs(candidate.key().fingerprint(), &requested)?;
        let mut ready = Vec::new();
        for graph in graphs {
            context.checkpoint()?;
            if locked_fingerprint.is_some_and(|expected| expected != graph.fingerprint()) {
                continue;
            }
            if let Err(error) = graph.validate_observed_pins(observed, &allowed) {
                if error
                    .downcast_ref::<crate::libraries::project::package_graph::GraphPinMismatch>()
                    .is_some()
                {
                    continue;
                }
                return Err(error);
            }
            let Some(layout) = crate::libraries::package::lease::acquire_layout_for_query(
                context,
                *graph.layout(),
            )?
            else {
                continue;
            };
            layout.check_metadata()?;
            if !crate::programs::install::reuse::matches_complete_roots(
                context,
                &graph,
                &layout,
                &complete_roots,
            )? {
                continue;
            }
            ready.push((graph, layout.bin(), layout.bin().join("python")));
            drop(layout);
        }
        if ready.is_empty() {
            return Ok(ObservedSelection {
                report: PythonQueryReport::MissingPackages {
                    requested: candidate.exact_version().to_owned(),
                    packages: roots.iter().map(|root| root.declaration.clone()).collect(),
                },
                candidate: Some(candidate),
                graph: None,
                already_applied: false,
            });
        }
        if ready.len() > 1 {
            return Ok(ObservedSelection {
                report: PythonQueryReport::Ambiguous {
                    requested: candidate.exact_version().to_owned(),
                    candidates: ready
                        .iter()
                        .map(|(graph, _, _)| format!("layout-{}", hex::encode(graph.layout())))
                        .collect(),
                },
                candidate: Some(candidate),
                graph: None,
                already_applied: false,
            });
        }
        let (graph, bin, executable) = ready.pop().expect("one Ready graph");
        selected.bin_dirs = vec![bin.clone()];
        selected.bin_dir = bin;
        selected.executable = executable;
        chosen_graph = Some(graph);
    }
    observed.recheck()?;
    context.checkpoint()?;
    repository.reload()?;
    let same_observation = repository
        .model()
        .project_registration(identity.as_uuid())
        .is_some_and(|registered| registered.declaration() == declaration);
    let current = repository.read_views().language_selection(&identity);
    let current_graph = repository.read_views().package_graph(&identity)?;
    let already_applied = same_observation
        && current.as_ref().is_some_and(|selection| {
            selection.language() == &candidate.key().fingerprint()
                && match &chosen_graph {
                    Some(graph) => current_graph
                        .as_ref()
                        .is_some_and(|current| current.layout() == graph.layout()),
                    None => {
                        selection.generation_allocation().is_none()
                            && selection.package_references().is_empty()
                    }
                }
        });
    // Description only. No object SH guard crosses the project-lock boundary.
    drop(language);
    drop(repository);
    Ok(ObservedSelection {
        report: PythonQueryReport::Ready(selected),
        candidate: Some(candidate),
        graph: chosen_graph,
        already_applied,
    })
}

fn query_conda_environment(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    candidate: LanguageCandidate,
    mut selected: DirectoryPython,
    language: &crate::libraries::language::lease::LanguageLease,
) -> Result<ObservedSelection> {
    let requested = observed.effective_packages()?.iter()
        .map(|item| format!("{}=={}", item.request.name, item.request.version))
        .collect::<Vec<_>>();
    let Some(model) = super::snapshot::read_project_lock_model(observed)? else {
        return Ok(ObservedSelection {
            report: PythonQueryReport::MissingPackages { requested: candidate.exact_version().into(), packages: requested },
            candidate: Some(candidate), graph: None, already_applied: false,
        });
    };
    crate::programs::install::lock_document::validate_declarations(observed, &model)?;
    let mut repository = ProjectRepository::open_readonly(context)?;
    let identity = ProjectIdentity::from_observation(observed)?;
    let graphs = repository.read_views().conda_package_graphs(candidate.key().fingerprint())?;
    let mut ready = Vec::new();
    for graph in graphs {
        let record = crate::packages::python_environment::model::PackageGraphRecord {
            roots: graph.roots().to_vec(), nodes: graph.nodes().to_vec(), edges: graph.edges().to_vec(),
        };
        if crate::programs::shared::commit::validate_conda_lock(observed, &model, language, &record).is_err() {
            continue;
        }
        let Some(layout) = crate::libraries::package::lease::acquire_layout_for_query(context, *graph.layout())? else { continue };
        layout.check_metadata()?;
        super::snapshot::validate_layout_graph(&graph, &layout)?;
        let activation = layout.activation_plan()?.map(|plan| (plan, layout.prefix().to_owned()));
        ready.push((graph, layout.bin(), layout.bin().join("python"), activation));
    }
    if ready.is_empty() {
        return Ok(ObservedSelection {
            report: PythonQueryReport::MissingPackages { requested: candidate.exact_version().into(), packages: requested },
            candidate: Some(candidate), graph: None, already_applied: false,
        });
    }
    if ready.len() > 1 {
        return Ok(ObservedSelection {
            report: PythonQueryReport::Ambiguous { requested: candidate.exact_version().into(),
                candidates: ready.iter().map(|(graph, _, _, _)| format!("layout-{}", hex::encode(graph.layout()))).collect() },
            candidate: Some(candidate), graph: None, already_applied: false,
        });
    }
    let (graph, bin, executable, activation) = ready.pop().expect("one Ready graph");
    selected.bin_dirs = vec![bin.clone()];
    selected.bin_dir = bin;
    selected.executable = executable;
    selected.activation = activation;
    repository.reload()?;
    let declaration = crate::programs::shared::commit::declaration_digest(observed);
    let same_observation = repository.model().project_registration(identity.as_uuid())
        .is_some_and(|registered| registered.declaration() == declaration);
    let current = repository.read_views().language_selection(&identity);
    let current_graph = repository.read_views().package_graph(&identity)?;
    let already_applied = same_observation
        && current.as_ref().is_some_and(|selection| selection.language() == &candidate.key().fingerprint())
        && current_graph.as_ref().is_some_and(|current| current.layout() == graph.layout());
    Ok(ObservedSelection { report: PythonQueryReport::Ready(selected), candidate: Some(candidate), graph: Some(graph), already_applied })
}
