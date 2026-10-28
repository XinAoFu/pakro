//! Bounded canonical TOML output over borrowed validated data.

use super::*;

struct Output {
    body: String,
    maximum: usize,
}
impl Output {
    fn raw(&mut self, value: &str) -> LockModelResult<()> {
        let end = self
            .body
            .len()
            .checked_add(value.len())
            .filter(|end| *end <= self.maximum)
            .ok_or(LockModelError::Limit(LockLimit::DocumentBytes))?;
        self.body
            .try_reserve(end - self.body.len())
            .map_err(|_| LockModelError::Limit(LockLimit::DocumentBytes))?;
        self.body.push_str(value);
        Ok(())
    }
    fn quoted(&mut self, value: &str) -> LockModelResult<()> {
        self.raw("\"")?;
        for ch in value.chars() {
            match ch {
                '"' => self.raw("\\\"")?,
                '\\' => self.raw("\\\\")?,
                '\t' => self.raw("\\t")?,
                '\n' => self.raw("\\n")?,
                '\r' => self.raw("\\r")?,
                ch if ch.is_control() => {
                    let value = format!("\\u{:04X}", u32::from(ch));
                    self.raw(&value)?;
                }
                ch => {
                    let mut bytes = [0; 4];
                    self.raw(ch.encode_utf8(&mut bytes))?;
                }
            }
        }
        self.raw("\"")
    }
    fn field(&mut self, key: &str, value: &str) -> LockModelResult<()> {
        self.raw(key)?;
        self.raw(" = ")?;
        self.quoted(value)?;
        self.raw("\n")
    }
    fn number(&mut self, key: &str, value: u64) -> LockModelResult<()> {
        self.raw(key)?;
        self.raw(" = ")?;
        self.raw(&value.to_string())?;
        self.raw("\n")
    }
    fn bytes(&mut self, key: &str, value: &[u8]) -> LockModelResult<()> {
        self.raw(key)?;
        self.raw(" = \"")?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in value {
            let pair = [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]];
            self.raw(std::str::from_utf8(&pair).expect("fixed hexadecimal ASCII"))?;
        }
        self.raw("\"\n")
    }
}

pub(super) fn projection(
    model: &PythonLockModel,
    limits: LockModelLimits,
) -> LockModelResult<String> {
    // Existing owned facts are rechecked under the caller's possibly tighter
    // limits without cloning the model or building an intermediate TOML tree.
    validate_owned(model, limits)?;
    let mut output = Output {
        body: String::new(),
        maximum: limits.max_document_bytes,
    };
    output.raw(
        if model.builds.iter().any(|(_, build)| build.native.is_some()) {
            "[python]\nschema = 4\n"
        } else if model.builds.is_empty() && model.declaration_fingerprint.is_none() {
            "[python]\nschema = 2\n"
        } else {
            "[python]\nschema = 3\n"
        },
    )?;
    if let Some(fingerprint) = &model.declaration_fingerprint {
        output.bytes("declaration_fingerprint", fingerprint)?;
    }
    if model.builds.is_empty() && model.declaration_fingerprint.is_some() {
        output.raw("builds = []\n")?;
    }
    if model.pins.is_empty() {
        output.raw("pins = []\n")?;
    }
    if model.graphs.is_empty() {
        output.raw("groups = []\n")?;
    }
    for pin in &model.pins {
        output.raw("\n[[python.pins]]\n")?;
        output.bytes("id", pin.id.as_bytes())?;
        output.field("registry", &pin.file.registry_id)?;
        output.bytes(
            "canonical_base_fingerprint",
            &pin.file.canonical_base_fingerprint,
        )?;
        output.field("project", &pin.file.project)?;
        output.field("version", &pin.file.version)?;
        output.field("filename", &pin.file.filename)?;
        output.field(
            "kind",
            match pin.file.kind {
                RegistryArtifactKind::Wheel => "wheel",
                RegistryArtifactKind::Sdist => "sdist",
            },
        )?;
        output.field("digest", &pin.digest.to_string())?;
        output.number("size", pin.size)?;
        output.field(
            "evidence",
            match pin.evidence {
                PinEvidence::Advertised => "advertised",
                PinEvidence::ObservedDuringFreshResolution => "observed-during-fresh-resolution",
            },
        )?;
    }
    for graph in &model.graphs {
        output.raw("\n[[python.groups]]\n")?;
        output.field("config_scope", &graph.key.config_scope)?;
        output.field("group", &graph.key.group)?;
        output.bytes("target", &graph.key.target)?;
        output.field(
            "marker_semantics",
            match graph.semantics {
                LockMarkerSemantics::Current2026 => "current2026",
            },
        )?;
        output.raw("root_declarations = [")?;
        for (index, declaration) in graph.root_declarations.iter().enumerate() {
            if index != 0 {
                output.raw(", ")?;
            }
            output.quoted(declaration)?;
        }
        output.raw("]\n")?;
        if graph.roots.is_empty() {
            output.raw("roots = []\n")?;
        }
        if graph.nodes.is_empty() {
            output.raw("nodes = []\n")?;
        }
        if graph.edges.is_empty() {
            output.raw("edges = []\n")?;
        }
        output.raw("[python.groups.runtime]\n")?;
        output.bytes("content_tree", &graph.runtime.content_tree)?;
        output.number("recipe", u64::from(graph.runtime.recipe))?;
        output.bytes("target", &graph.runtime.target)?;
        for root in &graph.roots {
            output.raw("[[python.groups.roots]]\n")?;
            output.number("declaration_index", root.declaration_index as u64)?;
            output.field("node", &root.node)?;
        }
        for node in &graph.nodes {
            output.raw("[[python.groups.nodes]]\n")?;
            output.field("name", &node.name)?;
            output.field("version", &node.version)?;
            output.bytes("source_pin", node.source_pin.as_bytes())?;
        }
        for edge in &graph.edges {
            output.raw("[[python.groups.edges]]\n")?;
            output.field("from", &edge.from)?;
            output.field("to", &edge.to)?;
            output.field("declaration", &edge.declaration)?;
        }
    }
    builds::write(&mut output, &model.builds)?;
    Ok(output.body)
}

#[path = "encode/builds.rs"]
mod builds;
