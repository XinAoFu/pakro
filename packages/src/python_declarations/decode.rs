//! Typed, budgeted collection over the existing TOML parser. No generic Value.
use super::*;
use pakro_index::python_requirements::{PythonRequirementError, parse_python_requirement};
use pep440_rs::{Version, VersionSpecifiers};
use regex::Regex;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::LazyLock;

type E = PythonDeclarationError;
type L = PythonDeclarationLimit;
type F = PythonDeclarationField;

pub(super) fn validate(limits: PythonProjectLimits) -> PythonDeclarationResult<()> {
    if [
        limits.max_document_bytes,
        limits.max_depth,
        limits.max_values,
        limits.max_field_bytes,
        limits.max_total_utf8_bytes,
        limits.max_requirements,
        limits.max_groups,
        limits.max_extras,
        limits.max_group_items,
        limits.max_backend_paths,
        limits.max_dynamic_fields,
        limits.max_config_entries,
        limits.max_config_values,
        limits.requirements.max_input_bytes,
        limits.requirements.max_requested_extras,
        limits.requirements.max_syntax_tokens,
        limits.requirements.max_marker_comparisons,
    ]
    .contains(&0)
        || limits.requirements.marker.validate().is_err()
    {
        return Err(E::InvalidLimits);
    }
    Ok(())
}
pub(super) fn requirement(
    raw: &str,
    limits: PythonRequirementLimits,
) -> PythonDeclarationResult<Arc<PythonRequirementSyntax>> {
    parse_python_requirement(raw, limits)
        .map(Arc::new)
        .map_err(|error| match error {
            PythonRequirementError::InvalidLimits => E::InvalidLimits,
            PythonRequirementError::LimitExceeded(_)
            | PythonRequirementError::Marker(
                pakro_index::python_markers::PythonMarkerError::LimitExceeded(_),
            ) => E::Limit(L::RequirementSyntax),
            _ => E::InvalidRequirement,
        })
}
struct Context {
    limits: PythonProjectLimits,
    values: usize,
    utf8: usize,
    requirements: usize,
    groups: usize,
    extras: usize,
    items: usize,
    paths: usize,
    dynamic: usize,
    error: Option<E>,
}
impl Context {
    fn new(limits: PythonProjectLimits) -> Self {
        Self {
            limits,
            values: 0,
            utf8: 0,
            requirements: 0,
            groups: 0,
            extras: 0,
            items: 0,
            paths: 0,
            dynamic: 0,
            error: None,
        }
    }
    fn fail<T: de::Error>(&mut self, error: E) -> T {
        if self.error.is_none() {
            self.error = Some(error);
        }
        T::custom("invalid Python declaration data")
    }
    fn limit(&mut self, count: usize, kind: L) -> PythonDeclarationResult<()> {
        let (slot, maximum) = match kind {
            L::Values => (&mut self.values, self.limits.max_values),
            L::Utf8Bytes => (&mut self.utf8, self.limits.max_total_utf8_bytes),
            L::Requirements => (&mut self.requirements, self.limits.max_requirements),
            L::Groups => (&mut self.groups, self.limits.max_groups),
            L::Extras => (&mut self.extras, self.limits.max_extras),
            L::GroupItems => (&mut self.items, self.limits.max_group_items),
            L::BackendPaths => (&mut self.paths, self.limits.max_backend_paths),
            L::DynamicFields => (&mut self.dynamic, self.limits.max_dynamic_fields),
            _ => return Err(E::InvalidLimits),
        };
        *slot = slot
            .checked_add(count)
            .filter(|n| *n <= maximum)
            .ok_or(E::Limit(kind))?;
        Ok(())
    }
    fn enter<T: de::Error>(&mut self, depth: usize) -> Result<(), T> {
        if depth > self.limits.max_depth {
            return Err(self.fail(E::Limit(L::Depth)));
        }
        self.limit(1, L::Values).map_err(|error| self.fail(error))
    }
    fn text<T: de::Error>(&mut self, text: &str) -> Result<(), T> {
        if text.len() > self.limits.max_field_bytes {
            return Err(self.fail(E::Limit(L::FieldBytes)));
        }
        self.limit(text.len(), L::Utf8Bytes)
            .map_err(|error| self.fail(error))
    }
    fn checked<T, X: de::Error>(&mut self, result: PythonDeclarationResult<T>) -> Result<T, X> {
        result.map_err(|error| self.fail(error))
    }
    fn typed<T, X>(&mut self, result: Result<T, X>, field: F) -> Result<T, X> {
        if result.is_err() && self.error.is_none() {
            self.error = Some(E::InvalidField(field));
        }
        result
    }
}
fn push<T, X: de::Error>(ctx: &mut Context, values: &mut Vec<T>, value: T) -> Result<(), X> {
    values
        .try_reserve(1)
        .map_err(|_| ctx.fail(E::Limit(L::Values)))?;
    values.push(value);
    Ok(())
}
struct Text<'a> {
    ctx: &'a mut Context,
    depth: usize,
    field: F,
}
impl<'de> DeserializeSeed<'de> for Text<'_> {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<String, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a>(&'a mut Context);
        impl Visitor<'_> for V<'_> {
            type Value = String;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a bounded declaration string")
            }
            fn visit_str<X: de::Error>(self, value: &str) -> Result<String, X> {
                self.0.text(value)?;
                Ok(value.to_owned())
            }
            fn visit_borrowed_str<X: de::Error>(self, value: &str) -> Result<String, X> {
                self.visit_str(value)
            }
            fn visit_string<X: de::Error>(self, value: String) -> Result<String, X> {
                self.0.text(&value)?;
                Ok(value)
            }
        }
        let result = de.deserialize_str(V(self.ctx));
        self.ctx.typed(result, self.field)
    }
}
struct Skip<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Skip<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        self.ctx.enter(self.depth)?;
        de.deserialize_any(SkipVisitor {
            ctx: self.ctx,
            depth: self.depth,
        })
    }
}
struct SkipVisitor<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> Visitor<'de> for SkipVisitor<'_> {
    type Value = ();
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded opaque TOML data")
    }
    fn visit_bool<X: de::Error>(self, _: bool) -> Result<(), X> {
        Ok(())
    }
    fn visit_i64<X: de::Error>(self, _: i64) -> Result<(), X> {
        Ok(())
    }
    fn visit_u64<X: de::Error>(self, _: u64) -> Result<(), X> {
        Ok(())
    }
    fn visit_f64<X: de::Error>(self, _: f64) -> Result<(), X> {
        Ok(())
    }
    fn visit_str<X: de::Error>(self, value: &str) -> Result<(), X> {
        self.ctx.text(value)
    }
    fn visit_borrowed_str<X: de::Error>(self, value: &str) -> Result<(), X> {
        self.visit_str(value)
    }
    fn visit_string<X: de::Error>(self, value: String) -> Result<(), X> {
        self.visit_str(&value)
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
        while seq
            .next_element_seed(Skip {
                ctx: self.ctx,
                depth: self.depth + 1,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
        while map
            .next_key_seed(Text {
                ctx: self.ctx,
                depth: self.depth + 1,
                field: F::Project,
            })?
            .is_some()
        {
            map.next_value_seed(Skip {
                ctx: self.ctx,
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
}
struct Requirements<'a> {
    ctx: &'a mut Context,
    depth: usize,
    field: F,
}
impl<'de> DeserializeSeed<'de> for Requirements<'_> {
    type Value = Vec<Arc<PythonRequirementSyntax>>;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
            field: F,
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = Vec<Arc<PythonRequirementSyntax>>;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a bounded requirement list")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element_seed(Requirement {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                    field: self.field,
                })? {
                    push(self.ctx, &mut values, value)?;
                }
                Ok(values)
            }
        }
        let result = de.deserialize_seq(V {
            ctx: self.ctx,
            depth: self.depth,
            field: self.field,
        });
        self.ctx.typed(result, self.field)
    }
}
struct Requirement<'a> {
    ctx: &'a mut Context,
    depth: usize,
    field: F,
}
impl<'de> DeserializeSeed<'de> for Requirement<'_> {
    type Value = Arc<PythonRequirementSyntax>;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx
            .limit(1, L::Requirements)
            .map_err(|e| self.ctx.fail(e))?;
        let raw = Text {
            ctx: self.ctx,
            depth: self.depth,
            field: self.field,
        }
        .deserialize(de)?;
        self.ctx
            .checked(requirement(&raw, self.ctx.limits.requirements))
    }
}
struct Texts<'a> {
    ctx: &'a mut Context,
    depth: usize,
    field: F,
    count: L,
}
impl<'de> DeserializeSeed<'de> for Texts<'_> {
    type Value = Vec<String>;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Vec<String>, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
            field: F,
            count: L,
        }
        struct Item<'a> {
            ctx: &'a mut Context,
            depth: usize,
            field: F,
            count: L,
        }
        impl<'de> DeserializeSeed<'de> for Item<'_> {
            type Value = String;
            fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<String, D::Error> {
                self.ctx
                    .limit(1, self.count)
                    .map_err(|e| self.ctx.fail(e))?;
                Text {
                    ctx: self.ctx,
                    depth: self.depth,
                    field: self.field,
                }
                .deserialize(de)
            }
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = Vec<String>;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a bounded string list")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Vec<String>, S::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element_seed(Item {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                    field: self.field,
                    count: self.count,
                })? {
                    push(self.ctx, &mut values, value)?;
                }
                Ok(values)
            }
        }
        let result = de.deserialize_seq(V {
            ctx: self.ctx,
            depth: self.depth,
            field: self.field,
            count: self.count,
        });
        self.ctx.typed(result, self.field)
    }
}
struct Extras<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Extras<'_> {
    type Value = BTreeMap<String, OptionalExtra>;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = BTreeMap<String, OptionalExtra>;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("optional dependency lists")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut out = BTreeMap::new();
                while let Some(original) = map.next_key_seed(Text {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                    field: F::OptionalDependencies,
                })? {
                    self.ctx.limit(1, L::Extras).map_err(|e| self.ctx.fail(e))?;
                    let name = self.ctx.checked(
                        NormalizedPythonName::new(&original)
                            .map_err(|_| E::InvalidField(F::OptionalDependencies)),
                    )?;
                    if out.contains_key(name.as_str()) {
                        return Err(self
                            .ctx
                            .fail(E::DuplicateName(PythonDeclarationNamespace::Extra)));
                    }
                    let requirements = map.next_value_seed(Requirements {
                        ctx: self.ctx,
                        depth: self.depth + 1,
                        field: F::OptionalDependencies,
                    })?;
                    out.insert(
                        name.as_str().to_owned(),
                        OptionalExtra {
                            original,
                            requirements,
                        },
                    );
                }
                Ok(out)
            }
        }
        let result = de.deserialize_map(V {
            ctx: self.ctx,
            depth: self.depth,
        });
        self.ctx.typed(result, F::OptionalDependencies)
    }
}
// A group item may contain a future extension. Keep a bounded invalid marker
// rather than eagerly failing an unrelated group; selected use reports it.
struct GroupItem<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for GroupItem<'_> {
    type Value = RawGroupItem;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        self.ctx
            .limit(1, L::GroupItems)
            .map_err(|e| self.ctx.fail(e))?;
        de.deserialize_any(GroupItemVisitor {
            ctx: self.ctx,
            depth: self.depth,
        })
    }
}
struct GroupItemVisitor<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
macro_rules! invalid_scalars {
    ($output:ty,$value:expr) => {
        fn visit_bool<X: de::Error>(self, _: bool) -> Result<$output, X> {
            Ok($value)
        }
        fn visit_i64<X: de::Error>(self, _: i64) -> Result<$output, X> {
            Ok($value)
        }
        fn visit_u64<X: de::Error>(self, _: u64) -> Result<$output, X> {
            Ok($value)
        }
        fn visit_f64<X: de::Error>(self, _: f64) -> Result<$output, X> {
            Ok($value)
        }
    };
}
impl<'de> Visitor<'de> for GroupItemVisitor<'_> {
    type Value = RawGroupItem;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded dependency group data")
    }
    invalid_scalars!(RawGroupItem, RawGroupItem::Invalid);
    fn visit_str<X: de::Error>(self, value: &str) -> Result<RawGroupItem, X> {
        self.ctx.text(value)?;
        self.ctx
            .limit(1, L::Requirements)
            .map_err(|e| self.ctx.fail(e))?;
        Ok(RawGroupItem::Requirement(value.to_owned()))
    }
    fn visit_borrowed_str<X: de::Error>(self, value: &str) -> Result<RawGroupItem, X> {
        self.visit_str(value)
    }
    fn visit_string<X: de::Error>(self, value: String) -> Result<RawGroupItem, X> {
        self.visit_str(&value)
    }
    fn visit_seq<S: SeqAccess<'de>>(self, seq: S) -> Result<RawGroupItem, S::Error> {
        SkipVisitor {
            ctx: self.ctx,
            depth: self.depth,
        }
        .visit_seq(seq)?;
        Ok(RawGroupItem::Invalid)
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<RawGroupItem, M::Error> {
        let mut count = 0usize;
        let mut target = None;
        while let Some(key) = map.next_key_seed(Text {
            ctx: self.ctx,
            depth: self.depth + 1,
            field: F::DependencyGroups,
        })? {
            count += 1;
            if key == "include-group" {
                target = map.next_value_seed(DeferredText {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                })?;
            } else {
                map.next_value_seed(Skip {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                })?;
            }
        }
        Ok(if count == 1 {
            target.map_or(RawGroupItem::Invalid, RawGroupItem::Include)
        } else {
            RawGroupItem::Invalid
        })
    }
}
struct DeferredText<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for DeferredText<'_> {
    type Value = Option<String>;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = Option<String>;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("bounded opaque group data")
            }
            invalid_scalars!(Option<String>, None);
            fn visit_str<X: de::Error>(self, value: &str) -> Result<Self::Value, X> {
                self.ctx.text(value)?;
                Ok(Some(value.to_owned()))
            }
            fn visit_borrowed_str<X: de::Error>(self, value: &str) -> Result<Self::Value, X> {
                self.visit_str(value)
            }
            fn visit_string<X: de::Error>(self, value: String) -> Result<Self::Value, X> {
                self.visit_str(&value)
            }
            fn visit_seq<S: SeqAccess<'de>>(self, seq: S) -> Result<Self::Value, S::Error> {
                SkipVisitor {
                    ctx: self.ctx,
                    depth: self.depth,
                }
                .visit_seq(seq)?;
                Ok(None)
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                SkipVisitor {
                    ctx: self.ctx,
                    depth: self.depth,
                }
                .visit_map(map)?;
                Ok(None)
            }
        }
        de.deserialize_any(V {
            ctx: self.ctx,
            depth: self.depth,
        })
    }
}
struct GroupContents<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for GroupContents<'_> {
    type Value = RawGroup;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = RawGroup;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("bounded dependency group contents")
            }
            invalid_scalars!(RawGroup, RawGroup::Invalid);
            fn visit_str<X: de::Error>(self, value: &str) -> Result<RawGroup, X> {
                self.ctx.text(value)?;
                Ok(RawGroup::Invalid)
            }
            fn visit_borrowed_str<X: de::Error>(self, value: &str) -> Result<RawGroup, X> {
                self.visit_str(value)
            }
            fn visit_string<X: de::Error>(self, value: String) -> Result<RawGroup, X> {
                self.visit_str(&value)
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<RawGroup, M::Error> {
                SkipVisitor {
                    ctx: self.ctx,
                    depth: self.depth,
                }
                .visit_map(map)?;
                Ok(RawGroup::Invalid)
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<RawGroup, S::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element_seed(GroupItem {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                })? {
                    push(self.ctx, &mut values, value)?;
                }
                Ok(RawGroup::Items(values))
            }
        }
        de.deserialize_any(V {
            ctx: self.ctx,
            depth: self.depth,
        })
    }
}
struct Groups<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Groups<'_> {
    type Value = BTreeMap<String, DependencyGroup>;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = BTreeMap<String, DependencyGroup>;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("dependency group definitions")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut out = BTreeMap::new();
                while let Some(original) = map.next_key_seed(Text {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                    field: F::DependencyGroups,
                })? {
                    self.ctx.limit(1, L::Groups).map_err(|e| self.ctx.fail(e))?;
                    let name = self.ctx.checked(
                        NormalizedPythonName::new(&original)
                            .map_err(|_| E::InvalidField(F::DependencyGroups)),
                    )?;
                    if out.contains_key(name.as_str()) {
                        return Err(self
                            .ctx
                            .fail(E::DuplicateName(PythonDeclarationNamespace::Group)));
                    }
                    let contents = map.next_value_seed(GroupContents {
                        ctx: self.ctx,
                        depth: self.depth + 1,
                    })?;
                    out.insert(
                        name.as_str().to_owned(),
                        DependencyGroup { original, contents },
                    );
                }
                Ok(out)
            }
        }
        let result = de.deserialize_map(V {
            ctx: self.ctx,
            depth: self.depth,
        });
        self.ctx.typed(result, F::DependencyGroups)
    }
}
const PROJECT_FIELDS: &[&str] = &[
    "name",
    "version",
    "description",
    "readme",
    "requires-python",
    "license",
    "license-files",
    "authors",
    "maintainers",
    "keywords",
    "classifiers",
    "urls",
    "scripts",
    "gui-scripts",
    "entry-points",
    "dependencies",
    "optional-dependencies",
    "import-names",
    "import-namespaces",
    "dynamic",
];
const APPEND_FIELDS: &[&str] = &[
    "authors",
    "classifiers",
    "dependencies",
    "entry-points",
    "gui-scripts",
    "import-names",
    "import-namespaces",
    "keywords",
    "license-files",
    "maintainers",
    "optional-dependencies",
    "scripts",
    "urls",
];
struct ProjectData {
    metadata: PythonProjectMetadata,
    dependencies: Vec<Arc<PythonRequirementSyntax>>,
    extras: BTreeMap<String, OptionalExtra>,
    dynamic: Vec<String>,
}
struct Project<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Project<'_> {
    type Value = ProjectData;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = ProjectData;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("project metadata declarations")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let (mut name, mut version, mut python) = (None, None, None);
                let (mut dependencies, mut extras, mut dynamic) =
                    (Vec::new(), BTreeMap::new(), Vec::new());
                let mut present = BTreeSet::new();
                while let Some(key) = map.next_key_seed(Text {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                    field: F::Project,
                })? {
                    if !PROJECT_FIELDS.contains(&key.as_str()) {
                        return Err(self.ctx.fail(E::InvalidField(F::Project)));
                    }
                    present.insert(key.clone());
                    match key.as_str() {
                        "name" => {
                            name = Some(map.next_value_seed(Text {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::Name,
                            })?)
                        }
                        "version" => {
                            version = Some(map.next_value_seed(Text {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::Version,
                            })?)
                        }
                        "requires-python" => {
                            python = Some(map.next_value_seed(Text {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::RequiresPython,
                            })?)
                        }
                        "dependencies" => {
                            dependencies = map.next_value_seed(Requirements {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::Dependencies,
                            })?
                        }
                        "optional-dependencies" => {
                            extras = map.next_value_seed(Extras {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                            })?
                        }
                        "dynamic" => {
                            dynamic = map.next_value_seed(Texts {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::Dynamic,
                                count: L::DynamicFields,
                            })?
                        }
                        _ => map.next_value_seed(Skip {
                            ctx: self.ctx,
                            depth: self.depth + 1,
                        })?,
                    }
                }
                for field in &dynamic {
                    if field == "name" {
                        return Err(self.ctx.fail(E::DynamicConflict));
                    }
                    if field == "dynamic" || !PROJECT_FIELDS.contains(&field.as_str()) {
                        return Err(self.ctx.fail(E::InvalidField(F::Dynamic)));
                    }
                    if present.contains(field) && !APPEND_FIELDS.contains(&field.as_str()) {
                        return Err(self.ctx.fail(E::DynamicConflict));
                    }
                }
                let original_name = name.ok_or_else(|| self.ctx.fail(E::InvalidField(F::Name)))?;
                let name = self.ctx.checked(
                    NormalizedPythonName::new(&original_name).map_err(|_| E::InvalidField(F::Name)),
                )?;
                if version.is_none() && !dynamic.iter().any(|f| f == "version") {
                    return Err(self.ctx.fail(E::InvalidField(F::Version)));
                }
                if version
                    .as_ref()
                    .is_some_and(|v| Version::from_str(v).is_err())
                {
                    return Err(self.ctx.fail(E::InvalidField(F::Version)));
                }
                if python
                    .as_ref()
                    .is_some_and(|v| VersionSpecifiers::from_str(v).is_err())
                {
                    return Err(self.ctx.fail(E::InvalidField(F::RequiresPython)));
                }
                Ok(ProjectData {
                    metadata: PythonProjectMetadata {
                        original_name,
                        name,
                        version,
                        requires_python: python,
                    },
                    dependencies,
                    extras,
                    dynamic,
                })
            }
        }
        let result = de.deserialize_map(V {
            ctx: self.ctx,
            depth: self.depth,
        });
        self.ctx.typed(result, F::Project)
    }
}
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:_|\p{XID_Start})\p{XID_Continue}*$")
        .expect("constant backend identifier grammar")
});
fn backend(raw: String) -> PythonDeclarationResult<PythonBuildBackend> {
    let (module, object) = raw
        .split_once(':')
        .map_or((raw.as_str(), None), |(m, o)| (m, Some(o)));
    if !module.split('.').all(|p| IDENTIFIER.is_match(p))
        || object.is_some_and(|o| !o.split('.').all(|p| IDENTIFIER.is_match(p)))
    {
        return Err(E::InvalidField(F::Backend));
    }
    Ok(PythonBuildBackend::Explicit {
        module: module.to_owned(),
        object: object.map(str::to_owned),
    })
}
struct Build<'a> {
    ctx: &'a mut Context,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Build<'_> {
    type Value = PythonBuildSystem;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(self.depth)?;
        struct V<'a> {
            ctx: &'a mut Context,
            depth: usize,
        }
        impl<'de> Visitor<'de> for V<'_> {
            type Value = PythonBuildSystem;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("build-system declarations")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let (mut requires, mut backend_ref, mut paths) = (None, None, Vec::new());
                while let Some(key) = map.next_key_seed(Text {
                    ctx: self.ctx,
                    depth: self.depth + 1,
                    field: F::BuildSystem,
                })? {
                    match key.as_str() {
                        "requires" => {
                            requires = Some(map.next_value_seed(Requirements {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::BuildRequires,
                            })?)
                        }
                        "build-backend" => {
                            backend_ref = Some(map.next_value_seed(Text {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::Backend,
                            })?)
                        }
                        "backend-path" => {
                            paths = map.next_value_seed(Texts {
                                ctx: self.ctx,
                                depth: self.depth + 1,
                                field: F::BackendPath,
                                count: L::BackendPaths,
                            })?
                        }
                        _ => return Err(self.ctx.fail(E::InvalidField(F::BuildSystem))),
                    }
                }
                let requires =
                    requires.ok_or_else(|| self.ctx.fail(E::InvalidField(F::BuildRequires)))?;
                let backend = match backend_ref {
                    Some(value) => self.ctx.checked(backend(value))?,
                    None => PythonBuildBackend::LegacyUnspecified,
                };
                let mut backend_paths = Vec::new();
                for path in paths {
                    if path.starts_with('/') || path.contains('\0') {
                        return Err(self.ctx.fail(E::InvalidField(F::BackendPath)));
                    }
                    backend_paths.push(PythonBackendPath(path));
                }
                Ok(PythonBuildSystem {
                    origin: PythonBuildSystemOrigin::Declared,
                    requires,
                    backend,
                    backend_paths,
                })
            }
        }
        let result = de.deserialize_map(V {
            ctx: self.ctx,
            depth: self.depth,
        });
        self.ctx.typed(result, F::BuildSystem)
    }
}
struct Root<'a> {
    ctx: &'a mut Context,
}
impl<'de> DeserializeSeed<'de> for Root<'_> {
    type Value = ParsedPythonProject;
    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        self.ctx.enter(1)?;
        struct V<'a>(&'a mut Context);
        impl<'de> Visitor<'de> for V<'_> {
            type Value = ParsedPythonProject;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a pyproject document")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let (mut project, mut groups, mut build) = (None, BTreeMap::new(), None);
                while let Some(key) = map.next_key_seed(Text {
                    ctx: self.0,
                    depth: 2,
                    field: F::Project,
                })? {
                    match key.as_str() {
                        "project" => {
                            project = Some(map.next_value_seed(Project {
                                ctx: self.0,
                                depth: 2,
                            })?)
                        }
                        "dependency-groups" => {
                            groups = map.next_value_seed(Groups {
                                ctx: self.0,
                                depth: 2,
                            })?
                        }
                        "build-system" => {
                            build = Some(map.next_value_seed(Build {
                                ctx: self.0,
                                depth: 2,
                            })?)
                        }
                        _ => map.next_value_seed(Skip {
                            ctx: self.0,
                            depth: 2,
                        })?,
                    }
                }
                let build =
                    match build {
                        Some(value) => value,
                        None => {
                            self.0
                                .limit(1, L::Requirements)
                                .map_err(|e| self.0.fail(e))?;
                            self.0.text("setuptools")?;
                            PythonBuildSystem {
                                origin: PythonBuildSystemOrigin::DefaultAbsentTable,
                                requires: vec![self.0.checked(requirement(
                                    "setuptools",
                                    self.0.limits.requirements,
                                ))?],
                                backend: PythonBuildBackend::LegacyUnspecified,
                                backend_paths: Vec::new(),
                            }
                        }
                    };
                let (project, dependencies, extras, dynamic) = match project {
                    Some(value) => (
                        Some(value.metadata),
                        value.dependencies,
                        value.extras,
                        value.dynamic,
                    ),
                    None => (None, Vec::new(), BTreeMap::new(), Vec::new()),
                };
                Ok(ParsedPythonProject {
                    project,
                    dependencies,
                    extras,
                    groups,
                    dynamic,
                    build,
                    requirement_limits: self.0.limits.requirements,
                })
            }
        }
        de.deserialize_map(V(self.ctx))
    }
}
pub(super) fn parse(
    raw: &str,
    limits: PythonProjectLimits,
) -> PythonDeclarationResult<ParsedPythonProject> {
    validate(limits)?;
    if raw.len() > limits.max_document_bytes {
        return Err(E::Limit(L::DocumentBytes));
    }
    let de = toml::de::Deserializer::parse(raw).map_err(|_| E::InvalidToml)?;
    let mut ctx = Context::new(limits);
    let result = Root { ctx: &mut ctx }.deserialize(de);
    result.map_err(|_| ctx.error.unwrap_or(E::InvalidToml))
}
