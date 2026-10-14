use std::fs;
use std::path::{Path, PathBuf};

use path_absolutize::Absolutize;

use eyre::{Context, ContextCompat, Result, bail};
use usage_rs::Args;

use crate::config::Config;
use crate::{dirs, file};

/// Manage mise projects
///
/// A project is a directory tree plus its mise.toml; these commands scaffold
/// that file (init/attach) and report what mise manages (ls/status). The
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
/// Writes a mise.toml (in PATH, default the current directory) declaring the
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

/// Bring an existing code directory under mise management
///
/// Writes a minimal mise.toml into the current directory when none exists.
/// Code files are never modified.
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectAttach {}

/// List registered projects on this machine
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectLs {}

/// Show what mise manages for the current project
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
        let config_path = root.join("mise.toml");
        if config_path.exists() {
            bail!(
                "{} already exists; edit it directly or run `mise project attach` elsewhere",
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
            body.push_str("# managed by mise\n");
        }
        file::write(&config_path, body)?;
        ProjectsIndex::open().register(&root)?;
        miseprintln!("created {}", config_path.display());
        Ok(())
    }
}

impl ProjectAttach {
    fn run(self) -> Result<()> {
        let root = std::env::current_dir()?;
        let config_path = root.join("mise.toml");
        if config_path.exists() {
            miseprintln!("{} already exists; nothing to do", config_path.display());
            return Ok(());
        }
        file::write(&config_path, "# managed by mise\n")?;
        ProjectsIndex::open().register(&root)?;
        miseprintln!("attached {}", config_path.display());
        Ok(())
    }
}

impl ProjectLs {
    fn run(self) -> Result<()> {
        for project in ProjectsIndex::open().load() {
            miseprintln!("{}", project.display());
        }
        Ok(())
    }
}

impl ProjectStatus {
    async fn run(self) -> Result<()> {
        let config = Config::get().await?;
        let root = config
            .project_root
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .context("no project root")?;
        miseprintln!("root: {}", root.display());
        let requests = config.package_requests();
        if requests.is_empty() {
            miseprintln!("packages: (none)");
        } else {
            miseprintln!("packages:");
            for request in requests {
                miseprintln!(
                    "  {}@{} {} {}",
                    request.runtime_name,
                    request.runtime_version,
                    request.name,
                    request.version
                );
            }
        }
        let lock = root.join("mise.packages.lock");
        miseprintln!(
            "lockfile: {}",
            if lock.exists() {
                lock.display().to_string()
            } else {
                "(none)".to_string()
            }
        );
        let venv = root.join(".mise/venvs");
        if venv.exists() {
            miseprintln!("venvs: {}", venv.display());
        }
        if root.join("node_modules").exists() {
            miseprintln!("node_modules: present");
        }
        Ok(())
    }
}
