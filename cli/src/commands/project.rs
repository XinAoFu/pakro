use std::fs;
use std::path::{Path, PathBuf};

use path_absolutize::Absolutize;

use eyre::{Context, Result, bail};
use usage_rs::Args;

use crate::{dirs, file};

/// Manage pakro projects
///
/// A project is a directory tree plus its pakro.toml; these commands scaffold
/// that file (init/attach) and report what pakro manages (ls/status). The
/// runtime association itself is unchanged: running a command resolves the
/// environment from the nearest config up the tree.
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Project {
    #[usage(subcommand)]
    command: ProjectCommands,
}

#[derive(Debug, usage_rs::Subcommands)]
enum ProjectCommands {
    Init(ProjectInit),
    Attach(ProjectAttach),
    Ls(ProjectLs),
    Status(ProjectStatus),
}

/// Create a new project scaffold
///
/// Writes a pakro.toml (in PATH, default the current directory) declaring the
/// requested runtimes and package pins. Never touches code files.
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectInit {
    /// Directory to scaffold; created when missing
    path: Option<PathBuf>,
    /// Declare a python runtime with an exact version
    #[usage(long)]
    python: Option<String>,
    /// Declare a node runtime with an exact version
    #[usage(long)]
    node: Option<String>,
    /// Package pin like python+numpy@1.26.4 (eco + NAME@VERSION)
    #[usage(long = "package", value_name = "ECO+NAME@VERSION")]
    packages: Vec<String>,
}

/// Bring an existing code directory under pakro management
///
/// Writes a minimal pakro.toml into the current directory when none exists.
/// Code files are never modified.
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectAttach {}

/// List registered projects on this machine
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectLs {}

/// Show what pakro manages for the current project
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectStatus {}

struct ProjectsIndex {
    path: PathBuf,
}

impl ProjectsIndex {
    fn open() -> Self {
        Self {
            path: dirs::STATE.join("projects.json"),
        }
    }

    fn load(&self) -> Vec<PathBuf> {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or_default()
    }

    fn register(&self, root: &Path) -> Result<()> {
        let mut projects = self.load();
        let root = root.absolutize()?.to_path_buf();
        if !projects.iter().any(|p| p == &root) {
            projects.push(root);
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        file::write(&self.path, serde_json::to_string_pretty(&projects)?)?;
        Ok(())
    }
}

impl Project {
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            ProjectCommands::Init(cmd) => cmd.run(),
            ProjectCommands::Attach(cmd) => cmd.run(),
            ProjectCommands::Ls(cmd) => cmd.run(),
            ProjectCommands::Status(cmd) => cmd.run().await,
        }
    }
}

impl ProjectInit {
    fn run(self) -> Result<()> {
        let root = match self.path {
            Some(path) => {
                fs::create_dir_all(&path)
                    .with_context(|| format!("creating {}", path.display()))?;
                path.absolutize()?.to_path_buf()
            }
            None => std::env::current_dir()?,
        };
        let config_path = root.join("pakro.toml");
        if config_path.exists() {
            bail!(
                "{} already exists; edit it directly or run `pakro project attach` elsewhere",
                config_path.display()
            );
        }
        let mut body = String::new();
        if self.python.is_some() || self.node.is_some() {
            body.push_str("[tools]\n");
            if let Some(python) = &self.python {
                body.push_str(&format!("python = \"{python}\"\n"));
            }
            if let Some(node) = &self.node {
                body.push_str(&format!("node = \"{node}\"\n"));
            }
        }
        for runtime in [
            ("python", self.python.as_deref()),
            ("node", self.node.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, version)| version.map(|v| (name, v)))
        {
            let mut pins = String::new();
            for pin in &self.packages {
                let Some((eco, name_version)) = pin.split_once('+') else {
                    bail!("package pin {pin:?} must be ECO+NAME@VERSION, e.g. python+numpy@1.26.4");
                };
                if eco != runtime.0 {
                    continue;
                }
                let Some((name, version)) = name_version.rsplit_once('@') else {
                    bail!("package pin {pin:?} must be ECO+NAME@VERSION");
                };
                pins.push_str(&format!("{name} = \"{version}\"\n"));
            }
            if pins.is_empty() {
                continue;
            }
            body.push_str(&format!(
                "\n[packages.\"{}@{}\"]\n{}",
                runtime.0, runtime.1, pins
            ));
        }
        if body.is_empty() {
            body.push_str("# managed by pakro\n");
        }
        file::write(&config_path, body)?;
        ProjectsIndex::open().register(&root)?;
        pakroprintln!("created {}", config_path.display());
        Ok(())
    }
}

impl ProjectAttach {
    fn run(self) -> Result<()> {
        let root = std::env::current_dir()?;
        let config_path = root.join("pakro.toml");
        if config_path.exists() {
            pakroprintln!("{} already exists; nothing to do", config_path.display());
            return Ok(());
        }
        file::write(&config_path, "# managed by pakro\n")?;
        ProjectsIndex::open().register(&root)?;
        pakroprintln!("attached {}", config_path.display());
        Ok(())
    }
}

impl ProjectLs {
    fn run(self) -> Result<()> {
        for project in crate::programs::library::projects()? {
            pakroprintln!(
                "{}  {}  python={}  packages={}  {}",
                project.identity,
                project.path.display(),
                project.language.as_deref().unwrap_or("(none)"),
                project.packages.join(","),
                if project.accessible {
                    "registered"
                } else {
                    "unavailable (references retained)"
                }
            );
        }
        Ok(())
    }
}

impl ProjectStatus {
    async fn run(self) -> Result<()> {
        let report = crate::programs::library::local_project()?;
        pakroprintln!("root: {}", report.root.display());
        if report.packages.is_empty() {
            pakroprintln!("packages: (none)");
        } else {
            pakroprintln!("packages:");
            for request in report.packages {
                pakroprintln!(
                    "  {}@{} {} {}",
                    request.runtime_name,
                    request.runtime_version,
                    request.name,
                    request.version
                );
            }
        }
        match report.registered {
            Some(project) => {
                pakroprintln!(
                    "registered: {} (revision {})",
                    project.identity,
                    project.revision
                );
                pakroprintln!(
                    "language: {}",
                    project.language.as_deref().unwrap_or("(none)")
                );
                pakroprintln!("selected packages: {}", project.packages.join(", "));
            }
            None => pakroprintln!("registered: (no committed selection)"),
        }
        if let Some(current) = report.current {
            pakroprintln!("python: {} ({})", current.requested, current.status);
            if let Some(detail) = current.detail {
                pakroprintln!("detail: {detail}");
            }
        }
        Ok(())
    }
}
