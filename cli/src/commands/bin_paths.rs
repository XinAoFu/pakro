use crate::args::ToolArg;
use crate::config::Config;
use crate::file;
use crate::toolset::ToolsetBuilder;
use eyre::Result;
use serde::Serialize;
use std::path::PathBuf;

/// List all the active runtime bin paths
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct BinPaths {
    /// Tool(s) to look up
    /// e.g.: ruby@3
    #[usage(value_name = "TOOL@VERSION", verbatim_doc_comment)]
    tool: Option<Vec<ToolArg>>,

    /// Output executable names instead of bin directories
    #[usage(long)]
    bin_names: bool,

    /// Output executable entries in JSON format (implies --bin-names)
    #[usage(long, short = 'J')]
    json: bool,
}

impl BinPaths {
    pub(super) fn explicitly_non_python(&self) -> bool {
        self.tool.as_ref().is_some_and(|tools| {
            !tools.is_empty()
                && tools
                    .iter()
                    .all(|tool| !crate::programs::python::is_python_backend(&tool.ba))
        })
    }

    pub(super) async fn requires_shared_python(&self) -> Result<bool> {
        if let Some(tools) = &self.tool {
            if !tools.is_empty() {
                return Ok(tools
                    .iter()
                    .any(|tool| crate::programs::python::is_python_backend(&tool.ba)));
            }
        }
        let config = Config::get().await?;
        let effective = ToolsetBuilder::new().build_unresolved(&config)?;
        Ok(effective.versions.is_empty()
            || effective
                .versions
                .keys()
                .any(|backend| crate::programs::python::is_python_backend(backend)))
    }

    pub(crate) async fn run(self) -> Result<()> {
        for tool in self.tool.iter().flatten() {
            crate::programs::python::validate_python_query_options(&tool.ba)?;
        }
        #[cfg(unix)]
        let shared_python = {
            use crate::programs::directory::{PythonQueryRequest, PythonQueryScope, query_python};
            let targets: Vec<_> = self
                .tool
                .iter()
                .flatten()
                .filter(|tool| crate::programs::python::is_python_backend(&tool.ba))
                .collect();
            eyre::ensure!(
                targets.len() <= 1,
                "bin-paths supports one explicit Python target per query"
            );
            if self.tool.is_none() || !targets.is_empty() {
                let selected = query_python(PythonQueryRequest {
                    target: targets.first().and_then(|tool| tool.version.clone()),
                    scope: PythonQueryScope::Environment,
                })?
                .into_ready()?;
                eyre::ensure!(
                    targets.is_empty() || selected.is_some(),
                    "no current Python declaration; name an installed Python version explicitly"
                );
                selected
            } else {
                None
            }
        };
        let config = Config::get().await?;
        let mut tsb = ToolsetBuilder::new();
        if let Some(tool) = &self.tool {
            tsb = tsb.with_args(tool);
        }
        let mut ts = tsb.build_unresolved(&config)?;
        #[cfg(unix)]
        ts.versions
            .retain(|backend, _| !crate::programs::python::is_python_backend(backend));
        if let Some(tool) = &self.tool {
            ts.versions.retain(|k, _| tool.iter().any(|t| *t.ba == **k));
        }
        ts.resolve_with_opts(
            &config,
            &crate::toolset::ResolveOptions {
                offline: self.tool.as_ref().is_none_or(|tools| {
                    tools
                        .iter()
                        .any(|tool| crate::programs::python::is_python_backend(&tool.ba))
                }),
                ..Default::default()
            },
        )
        .await?;
        ts.notify_if_versions_missing(&config).await;
        let mut paths = ts.list_paths_for_query(&config).await?;
        #[allow(unused_mut)]
        let mut verified_paths = std::collections::HashSet::new();
        #[cfg(unix)]
        if let Some(shared) = shared_python {
            verified_paths.extend(shared.bin_dirs.iter().cloned());
            paths.splice(0..0, shared.bin_dirs);
        }
        let mut seen = std::collections::HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        if self.bin_names || self.json {
            let bins = list_bins(paths, &verified_paths)?;
            if self.json {
                pakroprintln!("{}", serde_json::to_string_pretty(&bins)?);
            } else {
                for bin in bins {
                    pakroprintln!("{}", bin.name);
                }
            }
            return Ok(());
        }
        for p in paths {
            pakroprintln!("{}", p.display());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct BinPathEntry {
    name: String,
    path: PathBuf,
    symlink: bool,
}

fn list_bins(
    paths: Vec<PathBuf>,
    verified_paths: &std::collections::HashSet<PathBuf>,
) -> Result<Vec<BinPathEntry>> {
    let mut bins = vec![];
    for dir in paths {
        let entries = match dir.read_dir() {
            Ok(entries) => entries,
            Err(error) if verified_paths.contains(&dir) => return Err(error.into()),
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if verified_paths.contains(&dir) => return Err(error.into()),
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) if verified_paths.contains(&dir) => return Err(error.into()),
                Err(_) => continue,
            };
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }

            let path = entry.path();
            if !path.is_file() || !file::is_executable(&path) {
                continue;
            }

            bins.push(BinPathEntry {
                name: entry
                    .file_name()
                    .into_string()
                    .unwrap_or_else(|name| name.to_string_lossy().into_owned()),
                path,
                symlink: file_type.is_symlink(),
            });
        }
    }
    bins.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    bins.dedup_by(|a, b| a.name == b.name && a.path == b.path);
    Ok(bins)
}
