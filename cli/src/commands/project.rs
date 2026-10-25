use eyre::Result;
use usage_rs::Args;
/// Inspect registered projects and the current project's declarations and selection.
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
}
/// List registered projects on this machine
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectLs {}

/// Show what pakro manages for the current project
#[derive(Debug, Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct ProjectStatus {}

impl Project {
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            ProjectCommands::Ls(command) => command.run(),
            ProjectCommands::Status(command) => command.run().await,
        }
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
