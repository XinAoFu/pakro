//! Typed collection guards; no generic Value or unbounded derived DTO.

use super::*;
use serde::de::Error as _;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use std::marker::PhantomData;

struct Context {
    limits: LockModelLimits,
    usage: Usage,
    error: Option<LockModelError>,
}
impl Context {
    fn new(limits: LockModelLimits) -> Self {
        Self {
            limits,
            usage: Usage::default(),
            error: None,
        }
    }
    fn fail<E: de::Error>(&mut self, error: LockModelError) -> E {
        if self.error.is_none() {
            self.error = Some(error);
        }
        E::custom("invalid Python lock data")
    }
    fn checked<T, E: de::Error>(&mut self, result: LockModelResult<T>) -> Result<T, E> {
        result.map_err(|error| self.fail(error))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Python,
    Schema,
    Pins,
    Groups,
    Id,
    Registry,
    Base,
    Project,
    Version,
    Filename,
    Kind,
    Digest,
    Size,
    Evidence,
    Scope,
    Group,
    Target,
    Semantics,
    Runtime,
    RootDeclarations,
    Roots,
    Nodes,
    Edges,
    Tree,
    Recipe,
    Declaration,
    DeclarationIndex,
    Node,
    Name,
    Source,
    From,
    To,
    Unknown,
}
struct KeySeed;
impl<'de> DeserializeSeed<'de> for KeySeed {
    type Value = Key;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Key, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Key;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a Python lock field")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Key, E> {
                Ok(match value {
                    "python" => Key::Python,
                    "schema" => Key::Schema,
                    "pins" => Key::Pins,
                    "groups" => Key::Groups,
                    "id" => Key::Id,
                    "registry" => Key::Registry,
                    "canonical_base_fingerprint" => Key::Base,
                    "project" => Key::Project,
                    "version" => Key::Version,
                    "filename" => Key::Filename,
                    "kind" => Key::Kind,
                    "digest" => Key::Digest,
                    "size" => Key::Size,
                    "evidence" => Key::Evidence,
                    "config_scope" => Key::Scope,
                    "group" => Key::Group,
                    "target" => Key::Target,
                    "marker_semantics" => Key::Semantics,
                    "runtime" => Key::Runtime,
                    "root_declarations" => Key::RootDeclarations,
                    "roots" => Key::Roots,
                    "nodes" => Key::Nodes,
                    "edges" => Key::Edges,
                    "content_tree" => Key::Tree,
                    "recipe" => Key::Recipe,
                    "declaration" => Key::Declaration,
                    "declaration_index" => Key::DeclarationIndex,
                    "node" => Key::Node,
                    "name" => Key::Name,
                    "source_pin" => Key::Source,
                    "from" => Key::From,
                    "to" => Key::To,
                    _ => Key::Unknown,
                })
            }
        }
        de.deserialize_identifier(V)
    }
}

struct Fields<'a, 'de, M: MapAccess<'de>> {
    map: M,
    ctx: &'a mut Context,
    seen: u64,
    _de: PhantomData<&'de ()>,
}
impl<'a, 'de, M: MapAccess<'de>> Fields<'a, 'de, M> {
    fn new(map: M, ctx: &'a mut Context) -> Self {
        Self {
            map,
            ctx,
            seen: 0,
            _de: PhantomData,
        }
    }
    fn next(&mut self) -> Result<Option<Key>, M::Error> {
        let Some(key) = self.map.next_key_seed(KeySeed)? else {
            return Ok(None);
        };
        let bit = 1u64 << (key as u32);
        if key == Key::Unknown || self.seen & bit != 0 {
            return Err(self.bad());
        }
        self.seen |= bit;
        Ok(Some(key))
    }
    fn bad(&mut self) -> M::Error {
        self.ctx.fail(LockModelError::InvalidToml)
    }
    fn required<T>(&mut self, value: Option<T>) -> Result<T, M::Error> {
        value.ok_or_else(|| self.bad())
    }
    fn value<T: Wire>(&mut self) -> Result<T, M::Error> {
        self.map.next_value_seed(ValueSeed::<T> {
            ctx: self.ctx,
            _type: PhantomData,
        })
    }
    fn text(&mut self, mode: TextMode) -> Result<String, M::Error> {
        self.map.next_value_seed(TextSeed {
            ctx: self.ctx,
            mode,
        })
    }
    fn array<T: Wire>(&mut self, kind: ArrayKind) -> Result<Vec<T>, M::Error> {
        self.map.next_value_seed(ArraySeed::<T> {
            ctx: self.ctx,
            kind,
            _type: PhantomData,
        })
    }
}

trait Wire: Sized {
    fn read<'de, D: de::Deserializer<'de>>(ctx: &mut Context, de: D) -> Result<Self, D::Error>;
}
struct ValueSeed<'a, T> {
    ctx: &'a mut Context,
    _type: PhantomData<T>,
}
impl<'de, T: Wire> DeserializeSeed<'de> for ValueSeed<'_, T> {
    type Value = T;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<T, D::Error> {
        T::read(self.ctx, de)
    }
}

#[derive(Clone, Copy)]
enum TextMode {
    Plain(LockField),
    Name,
    Version,
    Declaration,
}
struct TextSeed<'a> {
    ctx: &'a mut Context,
    mode: TextMode,
}
impl TextSeed<'_> {
    fn make<E: de::Error>(&mut self, raw: &str) -> Result<String, E> {
        let field = match self.mode {
            TextMode::Plain(field) => field,
            TextMode::Name => LockField::Project,
            TextMode::Version => LockField::Version,
            TextMode::Declaration => LockField::Declaration,
        };
        let declaration = matches!(self.mode, TextMode::Declaration);
        let checked = text(
            raw,
            field,
            declaration,
            &mut self.ctx.usage,
            self.ctx.limits,
        );
        self.ctx.checked(checked)?;
        let value = match self.mode {
            TextMode::Name => self.ctx.checked(normalized(raw))?,
            TextMode::Version => self.ctx.checked(exact_version(raw))?,
            _ => raw.to_owned(),
        };
        let maximum = if declaration {
            self.ctx.limits.max_declaration_bytes
        } else {
            self.ctx.limits.max_field_bytes
        };
        let kind = if declaration {
            LockLimit::DeclarationBytes
        } else {
            LockLimit::FieldBytes
        };
        self.ctx.checked(count(value.len(), maximum, kind))?;
        if value.len() > raw.len() {
            let checked = charge(
                &mut self.ctx.usage.utf8,
                value.len() - raw.len(),
                self.ctx.limits.max_total_utf8_bytes,
                LockLimit::TotalUtf8Bytes,
            );
            self.ctx.checked(checked)?;
        }
        Ok(value)
    }
}
impl<'de> DeserializeSeed<'de> for TextSeed<'_> {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<String, D::Error> {
        struct V<'a>(TextSeed<'a>);
        impl Visitor<'_> for V<'_> {
            type Value = String;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a bounded Python lock string")
            }
            fn visit_str<E: de::Error>(mut self, value: &str) -> Result<String, E> {
                self.0.make(value)
            }
            fn visit_string<E: de::Error>(mut self, value: String) -> Result<String, E> {
                self.0.make(&value)
            }
        }
        de.deserialize_str(V(self))
    }
}

trait Atom: Sized {
    fn parse(value: &str) -> LockModelResult<Self>;
}
struct AtomVisitor<'a, T> {
    ctx: &'a mut Context,
    _type: PhantomData<T>,
}
impl<T: Atom> Visitor<'_> for AtomVisitor<'_, T> {
    type Value = T;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("a fixed Python lock value")
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<T, E> {
        self.ctx.checked(T::parse(value))
    }
}
macro_rules! atom {
    ($type:ty,$value:ident,$parse:expr) => {
        impl Atom for $type {
            fn parse($value: &str) -> LockModelResult<Self> {
                $parse
            }
        }
        impl Wire for $type {
            fn read<'de, D: de::Deserializer<'de>>(
                ctx: &mut Context,
                de: D,
            ) -> Result<Self, D::Error> {
                de.deserialize_str(AtomVisitor::<Self> {
                    ctx,
                    _type: PhantomData,
                })
            }
        }
    };
}
atom!([u8; 32], value, {
    let mut bytes = [0; 32];
    if value.len() != 64 || hex::decode_to_slice(value, &mut bytes).is_err() {
        return Err(LockModelError::InvalidField(LockField::PinId));
    }
    Ok(bytes)
});
atom!(PinId, value, PinId::parse_hex(value));
atom!(Digest, value, {
    if let Some(hex) = value.strip_prefix("sha256-") {
        let mut bytes = [0; 32];
        if hex.len() == 64 && hex::decode_to_slice(hex, &mut bytes).is_ok() {
            return Ok(Digest::Sha256(bytes));
        }
    } else if let Some(hex) = value.strip_prefix("sha512-") {
        let mut bytes = [0; 64];
        if hex.len() == 128 && hex::decode_to_slice(hex, &mut bytes).is_ok() {
            return Ok(Digest::Sha512(bytes));
        }
    }
    Err(LockModelError::InvalidField(LockField::Digest))
});
atom!(
    RegistryArtifactKind,
    value,
    match value {
        "wheel" => Ok(Self::Wheel),
        "sdist" => Ok(Self::Sdist),
        _ => Err(LockModelError::UnsupportedSourceKind),
    }
);
atom!(
    PinEvidence,
    value,
    match value {
        "advertised" => Ok(Self::Advertised),
        "observed-during-fresh-resolution" => Ok(Self::ObservedDuringFreshResolution),
        _ => Err(LockModelError::InvalidToml),
    }
);
atom!(
    LockMarkerSemantics,
    value,
    match value {
        "current2026" => Ok(Self::Current2026),
        _ => Err(LockModelError::UnsupportedMarkerSemantics),
    }
);
// Only the complete original-declarations array uses variable string items.
impl Wire for String {
    fn read<'de, D: de::Deserializer<'de>>(ctx: &mut Context, de: D) -> Result<Self, D::Error> {
        TextSeed {
            ctx,
            mode: TextMode::Declaration,
        }
        .deserialize(de)
    }
}
impl Wire for u64 {
    fn read<'de, D: de::Deserializer<'de>>(ctx: &mut Context, de: D) -> Result<Self, D::Error> {
        struct V<'a>(&'a mut Context);
        impl Visitor<'_> for V<'_> {
            type Value = u64;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a nonnegative lock integer")
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<u64, E> {
                Ok(value)
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<u64, E> {
                u64::try_from(value).map_err(|_| self.0.fail(LockModelError::InvalidToml))
            }
        }
        de.deserialize_u64(V(ctx))
    }
}

#[derive(Clone, Copy)]
enum ArrayKind {
    Pins,
    Groups,
    Nodes,
    RootDeclarations,
    Roots,
    Edges,
}
impl ArrayKind {
    fn bound(self, limits: LockModelLimits) -> (usize, LockLimit) {
        match self {
            Self::Pins => (limits.max_pins, LockLimit::Pins),
            Self::Groups => (limits.max_groups, LockLimit::Groups),
            Self::Nodes => (limits.max_nodes_per_group, LockLimit::Nodes),
            Self::RootDeclarations | Self::Roots => (limits.max_roots_per_group, LockLimit::Roots),
            Self::Edges => (limits.max_edges_per_group, LockLimit::Edges),
        }
    }
}
struct ArraySeed<'a, T> {
    ctx: &'a mut Context,
    kind: ArrayKind,
    _type: PhantomData<T>,
}
struct ItemSeed<'a, T> {
    ctx: &'a mut Context,
    kind: ArrayKind,
    count: &'a mut usize,
    _type: PhantomData<T>,
}
impl<'de, T: Wire> DeserializeSeed<'de> for ItemSeed<'_, T> {
    type Value = T;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<T, D::Error> {
        // SeqAccess calls this only for an actual item, including max+1. Charge
        // before entering that item's deserializer, not after a Vec collect.
        let (maximum, kind) = self.kind.bound(self.ctx.limits);
        let checked = charge(self.count, 1, maximum, kind);
        self.ctx.checked(checked)?;
        if matches!(self.kind, ArrayKind::Nodes) {
            let checked = charge(
                &mut self.ctx.usage.nodes,
                1,
                self.ctx.limits.max_total_nodes,
                LockLimit::TotalNodes,
            );
            self.ctx.checked(checked)?;
        }
        let references = match self.kind {
            ArrayKind::Nodes | ArrayKind::RootDeclarations => 1,
            ArrayKind::Roots | ArrayKind::Edges => 2,
            _ => 0,
        };
        let checked = charge(
            &mut self.ctx.usage.references,
            references,
            self.ctx.limits.max_total_references,
            LockLimit::References,
        );
        self.ctx.checked(checked)?;
        T::read(self.ctx, de)
    }
}
impl<'de, T: Wire> DeserializeSeed<'de> for ArraySeed<'_, T> {
    type Value = Vec<T>;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Vec<T>, D::Error> {
        struct V<'a, T>(ArraySeed<'a, T>);
        impl<'de, T: Wire> Visitor<'de> for V<'_, T> {
            type Value = Vec<T>;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a bounded Python lock array")
            }
            fn visit_seq<M: SeqAccess<'de>>(self, mut seq: M) -> Result<Vec<T>, M::Error> {
                let mut count = 0;
                let mut values = Vec::new();
                while let Some(value) = seq.next_element_seed(ItemSeed::<T> {
                    ctx: self.0.ctx,
                    kind: self.0.kind,
                    count: &mut count,
                    _type: PhantomData,
                })? {
                    values.try_reserve(1).map_err(|_| {
                        self.0.ctx.fail(LockModelError::Limit(
                            self.0.kind.bound(self.0.ctx.limits).1,
                        ))
                    })?;
                    values.push(value);
                }
                Ok(values)
            }
        }
        de.deserialize_seq(V(self))
    }
}

macro_rules! record {
    ($type:ty,$fields:ident,$body:block) => {
        impl Wire for $type {
            fn read<'de, D: de::Deserializer<'de>>(
                ctx: &mut Context,
                de: D,
            ) -> Result<Self, D::Error> {
                struct V<'a>(&'a mut Context);
                impl<'de> Visitor<'de> for V<'_> {
                    type Value = $type;
                    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                        out.write_str("a closed Python lock table")
                    }
                    fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                        let mut $fields = Fields::new(map, self.0);
                        $body
                    }
                }
                de.deserialize_map(V(ctx))
            }
        }
    };
}
record!(ContentPin, f, {
    let (
        mut id,
        mut registry,
        mut base,
        mut project,
        mut version,
        mut filename,
        mut kind,
        mut digest,
        mut size,
        mut evidence,
    ) = (None, None, None, None, None, None, None, None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::Id => id = Some(f.value::<PinId>()?),
            Key::Registry => registry = Some(f.text(TextMode::Plain(LockField::Registry))?),
            Key::Base => base = Some(f.value::<[u8; 32]>()?),
            Key::Project => project = Some(f.text(TextMode::Name)?),
            Key::Version => version = Some(f.text(TextMode::Version)?),
            Key::Filename => filename = Some(f.text(TextMode::Plain(LockField::Filename))?),
            Key::Kind => kind = Some(f.value::<RegistryArtifactKind>()?),
            Key::Digest => digest = Some(f.value::<Digest>()?),
            Key::Size => size = Some(f.value::<u64>()?),
            Key::Evidence => evidence = Some(f.value::<PinEvidence>()?),
            _ => return Err(f.bad()),
        }
    }
    let registry = f.required(registry)?;
    let project = f.required(project)?;
    let version = f.required(version)?;
    let filename = f.required(filename)?;
    let input = RegistryFileRefInput {
        registry_id: &registry,
        canonical_base_fingerprint: f.required(base)?,
        project: &project,
        version: &version,
        filename: &filename,
        kind: f.required(kind)?,
    };
    let checked = RegistryFileRef::new(input, f.ctx.limits);
    let file = f.ctx.checked(checked)?;
    let digest = f.required(digest)?;
    let size = f.required(size)?;
    let evidence = f.required(evidence)?;
    let checked = ContentPin::new(&file, digest, size, evidence, f.ctx.limits);
    let pin = f.ctx.checked(checked)?;
    if pin.id != f.required(id)? {
        return Err(f.ctx.fail(LockModelError::IdentityMismatch));
    }
    Ok(pin)
});
record!(RuntimePin, f, {
    let (mut tree, mut recipe, mut target) = (None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::Tree => tree = Some(f.value::<[u8; 32]>()?),
            Key::Recipe => recipe = Some(f.value::<u64>()?),
            Key::Target => target = Some(f.value::<[u8; 32]>()?),
            _ => return Err(f.bad()),
        }
    }
    let raw_recipe = f.required(recipe)?;
    let recipe = u32::try_from(raw_recipe)
        .map_err(|_| f.ctx.fail(LockModelError::UnsupportedRuntimeRecipe))?;
    let checked = RuntimePin::new(f.required(tree)?, recipe, f.required(target)?);
    f.ctx.checked(checked)
});
record!(LockedRoot, f, {
    let (mut declaration_index, mut node) = (None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::DeclarationIndex => declaration_index = Some(f.value::<u64>()?),
            Key::Node => node = Some(f.text(TextMode::Name)?),
            _ => return Err(f.bad()),
        }
    }
    Ok(LockedRoot {
        declaration_index: usize::try_from(f.required(declaration_index)?)
            .map_err(|_| f.ctx.fail(LockModelError::InvalidToml))?,
        node: f.required(node)?,
    })
});
record!(LockedNode, f, {
    let (mut name, mut version, mut source_pin) = (None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::Name => name = Some(f.text(TextMode::Name)?),
            Key::Version => version = Some(f.text(TextMode::Version)?),
            Key::Source => source_pin = Some(f.value::<PinId>()?),
            _ => return Err(f.bad()),
        }
    }
    Ok(LockedNode {
        name: f.required(name)?,
        version: f.required(version)?,
        source_pin: f.required(source_pin)?,
    })
});
record!(LockedEdge, f, {
    let (mut from, mut to, mut declaration) = (None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::From => from = Some(f.text(TextMode::Name)?),
            Key::To => to = Some(f.text(TextMode::Name)?),
            Key::Declaration => declaration = Some(f.text(TextMode::Declaration)?),
            _ => return Err(f.bad()),
        }
    }
    Ok(LockedEdge {
        from: f.required(from)?,
        to: f.required(to)?,
        declaration: f.required(declaration)?,
    })
});
record!(LockedGraph, f, {
    let (
        mut scope,
        mut group,
        mut target,
        mut semantics,
        mut runtime,
        mut root_declarations,
        mut roots,
        mut nodes,
        mut edges,
    ) = (None, None, None, None, None, None, None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::Scope => scope = Some(f.text(TextMode::Plain(LockField::Scope))?),
            Key::Group => group = Some(f.text(TextMode::Plain(LockField::Group))?),
            Key::Target => target = Some(f.value::<[u8; 32]>()?),
            Key::Semantics => semantics = Some(f.value::<LockMarkerSemantics>()?),
            Key::Runtime => runtime = Some(f.value::<RuntimePin>()?),
            Key::RootDeclarations => {
                root_declarations = Some(f.array::<String>(ArrayKind::RootDeclarations)?)
            }
            Key::Roots => roots = Some(f.array::<LockedRoot>(ArrayKind::Roots)?),
            Key::Nodes => nodes = Some(f.array::<LockedNode>(ArrayKind::Nodes)?),
            Key::Edges => edges = Some(f.array::<LockedEdge>(ArrayKind::Edges)?),
            _ => return Err(f.bad()),
        }
    }
    let scope = f.required(scope)?;
    let group = f.required(group)?;
    let target = f.required(target)?;
    let checked = GroupKey::new(&scope, &group, target, f.ctx.limits);
    let key = f.ctx.checked(checked)?;
    Ok(LockedGraph {
        key,
        semantics: f.required(semantics)?,
        runtime: f.required(runtime)?,
        root_declarations: f.required(root_declarations)?,
        roots: f.required(roots)?,
        nodes: f.required(nodes)?,
        edges: f.required(edges)?,
    })
});
record!(PythonLockModel, f, {
    let (mut schema, mut pins, mut groups) = (None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::Schema => {
                let value = f.value::<u64>()?;
                if value != 2 {
                    return Err(f.ctx.fail(LockModelError::UnsupportedSchema));
                }
                schema = Some(value);
            }
            Key::Pins => pins = Some(f.array::<ContentPin>(ArrayKind::Pins)?),
            Key::Groups => groups = Some(f.array::<LockedGraph>(ArrayKind::Groups)?),
            _ => return Err(f.bad()),
        }
    }
    if f.required(schema)? != 2 {
        return Err(f.ctx.fail(LockModelError::UnsupportedSchema));
    }
    let model = PythonLockModel {
        pins: f.required(pins)?,
        graphs: f.required(groups)?,
    };
    let checked = close_owned(model, f.ctx.limits);
    f.ctx.checked(checked)
});

struct ProjectionSeed<'a>(&'a mut Context);
impl<'de> DeserializeSeed<'de> for ProjectionSeed<'_> {
    type Value = PythonLockModel;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        struct V<'a>(&'a mut Context);
        impl<'de> Visitor<'de> for V<'_> {
            type Value = PythonLockModel;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a Python lock projection")
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                let mut f = Fields::new(map, self.0);
                let mut model = None;
                while let Some(key) = f.next()? {
                    if key != Key::Python {
                        return Err(f.bad());
                    }
                    model = Some(f.value::<PythonLockModel>()?);
                }
                f.required(model)
            }
        }
        de.deserialize_map(V(self.0))
    }
}

pub(super) fn projection(raw: &str, limits: LockModelLimits) -> LockModelResult<PythonLockModel> {
    valid_limits(limits)?;
    count(
        raw.len(),
        limits.max_document_bytes,
        LockLimit::DocumentBytes,
    )?;
    let de = toml::de::Deserializer::parse(raw).map_err(|_| LockModelError::InvalidToml)?;
    let mut ctx = Context::new(limits);
    ProjectionSeed(&mut ctx)
        .deserialize(de)
        .map_err(|_| ctx.error.take().unwrap_or(LockModelError::InvalidToml))
}

pub(super) fn table<'de, D: de::Deserializer<'de>>(
    de: D,
    limits: LockModelLimits,
) -> Result<PythonLockModel, D::Error> {
    if valid_limits(limits).is_err() {
        return Err(D::Error::custom("invalid Python lock limits"));
    }
    let mut ctx = Context::new(limits);
    // Discard third-party source-bearing errors, including their input/spans.
    // An enclosing root TOML adapter must likewise sanitize its outer error.
    PythonLockModel::read(&mut ctx, de).map_err(|_| D::Error::custom("invalid Python lock table"))
}
