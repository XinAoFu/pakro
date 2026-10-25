use crate::ui::table::PakroTable;
use comfy_table::Cell;
use eyre::Result;
/// List published shared language and package instances and their users.
#[derive(Debug, Default, usage_rs::Args)]
#[usage(visible_alias = "list", verbatim_doc_comment)]
pub(super) struct InstallsLs {
    /// Filter by language or package name.
    name: Vec<String>,
    #[usage(long, short = 'J')]
    json: bool,
    #[usage(long)]
    no_header: bool,
}
impl InstallsLs {
    pub(super) async fn run(self) -> Result<()> {
        let mut entries = crate::programs::library::inventory()?;
        if !self.name.is_empty() {
            entries.retain(|entry| self.name.contains(&entry.name));
        }
        if self.json {
            pakroprintln!("{}", serde_json::to_string_pretty(&entries)?);
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
        table.print()?;
        Ok(())
    }
}
