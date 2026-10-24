pub use pakro_util::tera::*;

/// Preserve the configured renderer and pure template functions while denying
/// command execution during local observation and startup target admission.
pub fn render_str(
    renderer: &mut TeraEngine,
    input: &str,
    context: &::tera::Context,
) -> ::tera::TeraResult<String> {
    if crate::config::readonly_invocation() {
        match renderer {
            TeraEngine::V2(renderer) => renderer.register_function(
                "exec",
                |_: ::tera::Kwargs, _: &::tera::State| -> ::tera::TeraResult<::tera::Value> {
                    Err(::tera::Error::message("exec() is unavailable during read-only target admission or local queries"))
                },
            ),
            TeraEngine::V1(renderer) => renderer.register_function(
                "exec",
                |_: &std::collections::HashMap<String, serde_json::Value>| -> ::tera1::Result<serde_json::Value> {
                    Err(::tera1::Error::msg("exec() is unavailable during read-only target admission or local queries"))
                },
            ),
        }
    }
    pakro_util::tera::render_str(renderer, input, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(s: &str) -> String {
        let config_root = std::path::Path::new("/");
        let mut tera_ctx = BASE_CONTEXT.clone();
        tera_ctx.insert("config_root", &config_root);
        tera_ctx.insert("cwd", "/");
        let mut tera = get_tera(Option::from(config_root));
        render_str(&mut tera, s, &tera_ctx).unwrap()
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_hash_file() {
        let s = render("{{ \"../fixtures/shorthands.toml\" | hash_file(len=64) }}");
        insta::assert_snapshot!(s, @"ce17f44735ea2083038e61c4b291ed31593e6cf4d93f5dc147e97e62962ac4e6");
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn test_canonicalize() {
        let s = render("{{ \"../fixtures/shorthands.toml\" | canonicalize }}");
        assert!(s.ends_with("/fixtures/shorthands.toml")); // test dir is not deterministic
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn test_file_size() {
        let s = render(r#"{{ "../fixtures/shorthands.toml" | file_size }}"#);
        assert_eq!(s, "48");
    }
    #[tokio::test]
    async fn test_last_modified() {
        let s = render(r#"{{ "../fixtures/shorthands.toml" | last_modified }}"#);
        let timestamp = s.parse::<u64>().unwrap();
        assert!((1725000000..=2725000000).contains(&timestamp));
    }
    #[tokio::test]
    async fn test_is_dir() {
        let s = render(r#"{% set p = ".pakro" %}{% if p is dir %} ok {% endif %}"#);
        assert_eq!(s.trim(), "ok");
    }
    #[tokio::test]
    async fn test_is_file() {
        let s = render(r#"{% set p = ".test-tool-versions" %}{% if p is file %} ok {% endif %}"#);
        assert_eq!(s.trim(), "ok");
    }
    #[tokio::test]
    async fn test_exists() {
        let s = render(r#"{% set p = ".test-tool-versions" %}{% if p is exists %} ok {% endif %}"#);
        assert_eq!(s.trim(), "ok");
    }
}
