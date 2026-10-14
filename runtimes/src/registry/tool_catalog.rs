use futures_util::future;
use itertools::Itertools;

use crate::backend::backend_type::BackendType;
use crate::backend::{cargo, dotnet, gem};
use crate::cache::CacheManagerBuilder;
use crate::config::{Settings, SettingsExt};
use crate::registry::{REGISTRY, RegistryTool, tool_enabled};
use crate::{dirs, timeout};

/// Built-in backends whose package registry has a search API.
const PACKAGE_REGISTRY_BACKENDS: &[BackendType] =
    &[BackendType::Cargo, BackendType::Dotnet, BackendType::Gem];
const SEARCH_LIMIT: usize = 20;
/// Per-registry limit when one search fans out to every registry, so a single
/// registry cannot fill the table.
const ALL_SEARCH_LIMIT: usize = 10;

#[derive(Debug, Clone)]
pub enum ToolCatalogSource {
    Registry(&'static RegistryTool),
    Backend,
}

#[derive(Debug, Clone)]
pub struct ToolCatalogEntry {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: ToolCatalogSource,
}

impl ToolCatalogEntry {
    pub fn canonical_id(&self) -> &str {
        match &self.source {
            ToolCatalogSource::Registry(tool) => tool.short,
            ToolCatalogSource::Backend => &self.id,
        }
    }

    pub fn selector_description(&self) -> &str {
        match &self.source {
            ToolCatalogSource::Registry(tool) => tool
                .description
                .or_else(|| tool.backends().first().copied())
                .unwrap_or_default(),
            ToolCatalogSource::Backend => self.description.as_deref().unwrap_or_default(),
        }
    }

    pub fn selectable(&self) -> bool {
        match &self.source {
            ToolCatalogSource::Registry(tool) => !tool.backends().is_empty(),
            ToolCatalogSource::Backend => true,
        }
    }
}

pub async fn search(_query: &str) -> Vec<ToolCatalogEntry> {
    let settings = Settings::get();
    let enable_tools = settings.enable_tools();
    let disable_tools = settings.disable_tools();
    let entries = REGISTRY
        .iter()
        .filter(|(short, _)| {
            tool_enabled(enable_tools.as_ref(), &disable_tools, &short.to_string())
        })
        .map(|(short, tool)| ToolCatalogEntry {
            id: short.to_string(),
            name: short.to_string(),
            description: tool.description.map(str::to_string),
            source: ToolCatalogSource::Registry(tool),
        })
        .collect_vec();

    entries
        .into_iter()
        .unique_by(|entry| entry.id.clone())
        .collect()
}

/// Searches built-in backends' package registries. A `backend:query` search,
/// e.g. `cargo:ripgrep`, searches that backend's registry. An unprefixed query
/// searches every registry when `all` is set and none otherwise, so plain
/// searches and shell completion never query package registries.
pub async fn search_package_registry(query: &str, all: bool) -> Vec<ToolCatalogEntry> {
    match query.split_once(':') {
        Some((backend, query)) => search_backend_registry(backend, query, SEARCH_LIMIT).await,
        None if all => {
            let backends = PACKAGE_REGISTRY_BACKENDS
                .iter()
                .map(ToString::to_string)
                .collect_vec();
            future::join_all(
                backends
                    .iter()
                    .map(|backend| search_backend_registry(backend, query, ALL_SEARCH_LIMIT)),
            )
            .await
            .concat()
        }
        None => vec![],
    }
}

async fn search_backend_registry(
    backend: &str,
    query: &str,
    limit: usize,
) -> Vec<ToolCatalogEntry> {
    let settings = Settings::get();
    let backend_type = BackendType::guess(backend);
    if query.is_empty()
        || !PACKAGE_REGISTRY_BACKENDS.contains(&backend_type)
        || settings.offline()
        || settings.disable_backends.iter().any(|b| b == backend)
        || (backend_type.is_experimental() && !settings.experimental)
        // crates.io results would not be installable from another registry
        || (backend_type == BackendType::Cargo && settings.cargo.registry_name.is_some())
    {
        return vec![];
    }
    // Results depend on which registry is configured, not just the query
    let registry = match backend_type {
        BackendType::Dotnet => settings.dotnet.registry_url.clone(),
        _ => String::new(),
    };
    let cache = CacheManagerBuilder::new(
        dirs::CACHE
            .join("package-registry-search")
            .join(format!("{backend}.msgpack.z")),
    )
    .with_cache_key(registry)
    .with_cache_key(query.to_string())
    .with_fresh_duration(settings.fetch_remote_versions_cache())
    .build();
    let result = cache
        .get_or_try_init_async(|| async {
            timeout::run_with_timeout_async(
                || async {
                    match backend_type {
                        BackendType::Cargo => cargo::search_tools(query, SEARCH_LIMIT).await,
                        BackendType::Dotnet => dotnet::search_tools(query, SEARCH_LIMIT).await,
                        BackendType::Gem => gem::search_tools(query, SEARCH_LIMIT).await,
                        _ => Ok(vec![]),
                    }
                },
                settings.fetch_remote_versions_timeout(),
            )
            .await
        })
        .await;
    let tools = match result {
        Ok(tools) => tools.iter().take(limit).cloned().collect(),
        Err(err) => {
            warn!("failed to search {backend} packages for {query}: {err:#}");
            return vec![];
        }
    };
    backend_entries(backend, tools).collect()
}

fn backend_entries(
    backend: &str,
    tools: Vec<crate::backend::BackendTool>,
) -> impl Iterator<Item = ToolCatalogEntry> + '_ {
    let settings = Settings::get();
    let enable_tools = settings.enable_tools();
    let disable_tools = settings.disable_tools();
    tools.into_iter().filter_map(move |tool| {
        let name = tool.name.trim();
        if !valid_tool_name(name) {
            debug!("ignoring invalid tool name from backend {backend}: {name:?}");
            return None;
        }
        let id = format!("{backend}:{name}");
        if !tool_enabled(enable_tools.as_ref(), &disable_tools, &id) {
            return None;
        }
        Some(ToolCatalogEntry {
            id,
            name: name.to_string(),
            // Registry descriptions can span several lines, which would break
            // table rows and completion output, and are publisher-controlled, so
            // control characters could inject terminal escape sequences.
            description: tool
                .description
                .map(|description| {
                    description
                        .split_whitespace()
                        .join(" ")
                        .replace(char::is_control, "")
                })
                .filter(|description| !description.is_empty()),
            source: ToolCatalogSource::Backend,
        })
    })
}

fn valid_tool_name(name: &str) -> bool {
    let valid_at = !name.contains('@')
        || name
            .strip_prefix('@')
            .is_some_and(|scoped| !scoped.contains('@'));
    !name.is_empty()
        && valid_at
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
        && !name.contains([':', '[', ']'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_tool_name() {
        assert!(valid_tool_name("prettier"));
        assert!(valid_tool_name("@scope/tool"));
        assert!(!valid_tool_name(""));
        assert!(!valid_tool_name("other:tool"));
        assert!(!valid_tool_name("two tools"));
        assert!(!valid_tool_name("tool\u{1b}[31m"));
        assert!(!valid_tool_name("tool[option=true]"));
        assert!(!valid_tool_name("tool@version"));
        assert!(!valid_tool_name("@scope/tool@version"));
    }

    #[tokio::test]
    async fn test_search_package_registry_skips_without_network() {
        // None of these reach a package registry, so they must return
        // immediately with no results.
        for query in ["", "prettier", "npm:", "pipx:black", "github:jdx/mise"] {
            assert!(
                search_package_registry(query, false).await.is_empty(),
                "{query}"
            );
        }
        assert!(search_package_registry("npm:", true).await.is_empty());
    }

    #[test]
    fn test_backend_entries_normalizes_descriptions() {
        let tools = vec![
            crate::backend::BackendTool {
                name: "rubocop".into(),
                description: Some("A linter.\n  It formats too.".into()),
            },
            crate::backend::BackendTool {
                name: "escape".into(),
                description: Some("\u{1b}]8;;https://evil\u{7}Nice tool".into()),
            },
            crate::backend::BackendTool {
                name: "blank".into(),
                description: Some(" \n".into()),
            },
            crate::backend::BackendTool {
                name: "bad name".into(),
                description: None,
            },
        ];
        let entries = backend_entries("gem", tools).collect_vec();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, "gem:rubocop");
        assert_eq!(
            entries[0].description.as_deref(),
            Some("A linter. It formats too.")
        );
        assert_eq!(
            entries[1].description.as_deref(),
            Some("]8;;https://evilNice tool")
        );
        assert_eq!(entries[2].description, None);
    }
}
