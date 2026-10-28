use eyre::Result;
use usage_rs::Args;
/// Inspect project selections or explicitly forget one registered project.
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Project {
    #[usage(subcommand)]
    command: ProjectCommands,
}
#[derive(Debug, usage_rs::Subcommands)]
enum ProjectCommands {
    Ls(ProjectLs),
    Status(ProjectStatus),
    Forget(ProjectForget),
}
/// List registered projects on this machine
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectLs {}

/// Show what pakro manages for the current project
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectStatus {}

/// Release a project's registered associations without deleting its files or shared contents.
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectForget {
    #[usage(value_name = "PROJECT_ID")]
    identity: String,
    /// Show the recorded path and associations without changing them.
    #[usage(long, short = 'n')]
    dry_run: bool,
}

impl Project {
    pub(super) fn is_readonly(&self) -> bool {
        !matches!(&self.command, ProjectCommands::Forget(command) if !command.dry_run)
    }

    pub(super) fn is_catalog_only(&self) -> bool {
        matches!(&self.command, ProjectCommands::Forget(_))
    }

    pub(super) fn inherit_root_dry_run(&mut self, dry_run: bool) {
        if let ProjectCommands::Forget(command) = &mut self.command {
            command.dry_run |= dry_run;
        }
    }

    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            ProjectCommands::Ls(command) => command.run(),
            ProjectCommands::Status(command) => command.run().await,
            ProjectCommands::Forget(command) => command.run(),
        }
    }
}
impl ProjectForget {
    fn run(self) -> Result<()> {
        let report = crate::programs::library::forget_project(&self.identity, self.dry_run)?;
        if report.already_forgotten {
            pakroprintln!(
                "Project {} has no registered associations; no changes",
                report.identity
            );
            return Ok(());
        }
        pakroprintln!(
            "{} project {}",
            if report.dry_run {
                "Would forget"
            } else {
                "Forgot"
            },
            report.identity
        );
        if let Some(path) = report.path {
            pakroprintln!("path: {}", path.display());
        }
        for association in report.associations {
            pakroprintln!("  {association}");
        }
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
        } else {
            pakroprintln!("python: (no current declaration)");
        }
        Ok(())
    }
}
