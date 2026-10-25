use eyre::Result;
mod path;
/// Inspect the local shared libraries and current project environment.
#[derive(Debug, usage_rs::Args)]
#[usage(visible_alias = "dr", verbatim_doc_comment)]
pub(crate) struct Doctor {
    #[usage(subcommand)]
    subcommand: Option<Commands>,
    #[usage(long, short = 'J')]
    json: bool,
}
#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    Path(path::Path),
}
impl Doctor {
    pub(crate) async fn run(self) -> Result<()> {
        if let Some(Commands::Path(command)) = self.subcommand {
            return command.run().await;
        }
        let mut errors = Vec::new();
        let entries = match crate::programs::library::inventory() {
            Ok(entries) => Some(entries),
            Err(error) => {
                errors.push(format!("shared library: {error:#}"));
                None
            }
        };
        let environment = match crate::programs::directory::query_python(
            crate::programs::directory::PythonQueryRequest {
                target: None,
                scope: crate::programs::directory::PythonQueryScope::Environment,
            },
        ) {
            Ok(report) => {
                use crate::programs::directory::PythonQueryReport;
                if !matches!(
                    report,
                    PythonQueryReport::Ready(_) | PythonQueryReport::NoDeclaration
                ) {
                    errors.push(report.to_string());
                }
                report.to_string()
            }
            Err(error) => {
                let message = format!("project: {error:#}");
                errors.push(message.clone());
                message
            }
        };
        if self.json {
            pakroprintln!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "version": crate::version::VERSION.to_string(),
                    "activated": crate::env::is_activated(),
                    "environment": environment,
                    "inventory": entries,
                    "errors": errors,
                }))?
            );
        } else {
            pakroprintln!("Pakro {}", crate::version::VERSION.as_str());
            pakroprintln!("{environment}");
            if let Some(entries) = &entries {
                pakroprintln!("Verified {} shared library instances", entries.len());
            }
            for error in &errors {
                pakroprintln!("ERROR: {error}");
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(crate::request_exit(1))
        }
    }
}
