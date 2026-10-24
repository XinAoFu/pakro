use comfy_table::Cell;
use eyre::Result;

use crate::args::BackendArg;
use crate::file::display_path;
use crate::install_layout::resolver::{self, Installation};
use crate::ui::table::PakroTable;

/// List published language and package library instances and their users
///
/// Every exact build and package variant is shown separately. This local
/// query does not download content, register projects or repair damaged data.
#[derive(Debug, Default, usage_rs::Args)]
#[usage(
    visible_alias = "list",
    example(
        r###"pakro installs ls"###,
        help = r###"Every installation in the identity layout"###
    ),
    example(
        r###"pakro installs ls node --json"###,
        help = r###"The installations of node, as JSON"###
    ),
    verbatim_doc_comment
)]
pub(super) struct InstallsLs {
    /// Only show the installations of these tools
    #[usage(value_name = "TOOL")]
    tool: Vec<BackendArg>,

    /// Output in JSON format
    #[usage(long, short = 'J')]
    json: bool,

    /// Don't display headers
    #[usage(long)]
    no_header: bool,
}

impl InstallsLs {
    pub(super) fn explicitly_non_python(&self) -> bool {
        !self.tool.is_empty()
            && self
                .tool
                .iter()
                .all(|tool| !crate::programs::python::is_python_backend(tool))
    }

    pub(super) async fn run(self) -> Result<()> {
        for tool in &self.tool {
            crate::programs::python::validate_python_query_options(tool)?;
        }
        // Explicit legacy tools retain their existing query even when a
        // shared Python library also exists (or is currently damaged).
        if !self.tool.is_empty()
            && self
                .tool
                .iter()
                .all(|tool| crate::programs::library::is_known_legacy_tool(tool))
        {
            return self.legacy().await;
        }
        let mut entries = crate::programs::library::inventory()?;
        let mut legacy = resolver::installations();
        legacy.retain(|installation| {
            installation.backend != "python"
                && (self.tool.is_empty()
                    || self
                        .tool
                        .iter()
                        .any(|tool| resolver::installation_belongs_to(tool, installation)))
        });
        if !self.tool.is_empty() {
            entries.retain(|entry| {
                self.tool.iter().any(|tool| {
                    tool.short == entry.name
                        || (entry.name == "python"
                            && crate::programs::python::is_python_backend(tool))
                })
            });
        }
        if self.json {
            let mut rows: Vec<_> = entries
                .iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<_, _>>()?;
            rows.extend(
                legacy
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<std::result::Result<Vec<_>, _>>()?,
            );
            pakroprintln!("{}", serde_json::to_string_pretty(&rows)?);
            return Ok(());
        }
        let mut table = PakroTable::new(
            self.no_header,
            &[
                "Instance", "Kind", "Name", "Version", "Build", "Platform", "Users",
            ],
        );
        for entry in entries {
            table.add_row(vec![
                Cell::new(entry.instance),
                Cell::new(entry.kind),
                Cell::new(entry.name),
                Cell::new(entry.version),
                Cell::new(entry.build.unwrap_or_default()),
                Cell::new(entry.platform),
                Cell::new(entry.users.join("\n")),
            ]);
        }
        for installation in &legacy {
            table.add_row(vec![
                Cell::new(&installation.name),
                Cell::new("legacy tool"),
                Cell::new(tool_label(installation)),
                Cell::new(&installation.version),
                Cell::new(""),
                Cell::new(&installation.platform),
                Cell::new(status(installation)),
            ]);
        }
        table.print()?;
        Ok(())
    }

    async fn legacy(self) -> Result<()> {
        let mut installations = resolver::installations();
        if !self.tool.is_empty() {
            installations.retain(|i| {
                self.tool
                    .iter()
                    .any(|ba| resolver::installation_belongs_to(ba, i))
            });
        }
        if self.json {
            pakroprintln!("{}", serde_json::to_string_pretty(&installations)?);
            return Ok(());
        }
        let mut table = PakroTable::new(
            self.no_header,
            &["Installation", "Tool", "Version", "Platform", "Status"],
        );
        for i in &installations {
            table.add_row(vec![
                Cell::new(&i.name),
                Cell::new(tool_label(i)),
                Cell::new(&i.version),
                Cell::new(&i.platform),
                Cell::new(status(i)),
            ]);
        }
        table.print()?;
        if installations.iter().any(|i| i.shared) {
            for i in installations.iter().filter(|i| i.shared) {
                info!("{} is in {}", i.name, display_path(&i.dir));
            }
        }
        Ok(())
    }
}

/// The tool an installation was requested as, with the options that set it
/// apart from other installations of the same version.
fn tool_label(i: &Installation) -> String {
    let tool = i.requested_as.clone().unwrap_or_else(|| i.backend.clone());
    if i.options.is_empty() {
        return tool;
    }
    let options = i
        .options
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{tool}[{options}]")
}

fn status(i: &Installation) -> String {
    [
        (i.selected, "selected"),
        (i.pinned, "pinned"),
        (i.shared, "shared"),
    ]
    .into_iter()
    .filter_map(|(on, word)| on.then_some(word))
    .collect::<Vec<_>>()
    .join(", ")
}
