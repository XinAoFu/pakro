//! Owned marker context data shared by declaration input and graph checking.

use pakro_index::python_markers::PythonMarkerScope;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnedRootMarkerScope {
    Environment,
    ProjectExtras(Vec<String>),
    Lock {
        extras: Vec<String>,
        groups: Vec<String>,
    },
}
impl OwnedRootMarkerScope {
    pub(crate) fn values(&self) -> impl Iterator<Item = &str> {
        let (extras, groups): (&[String], &[String]) = match self {
            Self::Environment => (&[], &[]),
            Self::ProjectExtras(extras) => (extras, &[]),
            Self::Lock { extras, groups } => (extras, groups),
        };
        extras.iter().chain(groups).map(String::as_str)
    }
    pub(crate) fn with_scope<T>(&self, use_scope: impl FnOnce(PythonMarkerScope<'_>) -> T) -> T {
        match self {
            Self::Environment => use_scope(PythonMarkerScope::Environment),
            Self::ProjectExtras(values) => {
                let values = values.iter().map(String::as_str).collect::<Vec<_>>();
                use_scope(PythonMarkerScope::Dependency {
                    requested_extras: &values,
                })
            }
            Self::Lock { extras, groups } => {
                let extras = extras.iter().map(String::as_str).collect::<Vec<_>>();
                let groups = groups.iter().map(String::as_str).collect::<Vec<_>>();
                use_scope(PythonMarkerScope::Lock {
                    selected_extras: &extras,
                    dependency_groups: &groups,
                })
            }
        }
    }
}
