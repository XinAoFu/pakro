//! Graph fields use the same pre-allocation collection/text budgets as the
//! original catalog codec. No unbounded intermediate wire model is created.
use super::read::{Element, List, ParseState, Text};
use super::*;
use serde::de::{DeserializeSeed, Deserializer, MapAccess, Visitor};
use std::fmt;

pub(super) struct Graph<'a>(pub &'a mut ParseState);
impl<'de> DeserializeSeed<'de> for Graph<'_> {
    type Value = PackageGraphRecord;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Graph<'_> {
    type Value = PackageGraphRecord;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("closed package graph")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut roots, mut nodes, mut edges, mut seen) = (None, None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "roots" => {
                    self.0.once(&mut seen, 1)?;
                    let maximum = self.0.limits.max_refs_per_generation;
                    roots = Some(map.next_value_seed(List::<usize>::new(self.0, maximum))?);
                }
                "nodes" => {
                    self.0.once(&mut seen, 2)?;
                    let maximum = self.0.limits.max_refs_per_generation;
                    nodes =
                        Some(map.next_value_seed(List::<PackageGraphNode>::new(self.0, maximum))?);
                }
                "edges" => {
                    self.0.once(&mut seen, 4)?;
                    let maximum = self.0.limits.max_total_references;
                    edges =
                        Some(map.next_value_seed(List::<PackageGraphEdge>::new(self.0, maximum))?);
                }
                _ => return self.0.fail("unknown package graph field"),
            }
        }
        let graph = PackageGraphRecord {
            roots: self.0.required(roots)?,
            nodes: self.0.required(nodes)?,
            edges: self.0.required(edges)?,
        };
        if graph.validate(self.0.limits).is_err() {
            return self.0.fail("invalid package graph");
        }
        Ok(graph)
    }
}
impl Element for usize {
    fn parse<'de, D: Deserializer<'de>>(
        _state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        serde::Deserialize::deserialize(input)
    }
}
struct Node<'a>(&'a mut ParseState);
impl Element for PackageGraphNode {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        input.deserialize_map(Node(state))
    }
}
impl<'de> Visitor<'de> for Node<'_> {
    type Value = PackageGraphNode;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("exact package node")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut name, mut version, mut instance, mut source_pin, mut artifact, mut pin, mut seen) =
            (None, None, None, None, None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "name" => {
                    self.0.once(&mut seen, 1)?;
                    name = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 1024,
                    })?);
                }
                "version" => {
                    self.0.once(&mut seen, 2)?;
                    version = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 1024,
                    })?);
                }
                "instance" => {
                    self.0.once(&mut seen, 4)?;
                    instance = Some(map.next_value::<[u8; 32]>()?);
                }
                "source_pin" => {
                    self.0.once(&mut seen, 8)?;
                    source_pin = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 64,
                    })?);
                }
                "artifact" => {
                    self.0.once(&mut seen, 16)?;
                    artifact = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 135,
                    })?);
                }
                "pin" => {
                    self.0.once(&mut seen, 32)?;
                    pin = Some(map.next_value_seed(Pin(self.0))?);
                }
                _ => return self.0.fail("unknown package node field"),
            }
        }
        Ok(PackageGraphNode {
            name: self.0.required(name)?,
            version: self.0.required(version)?,
            instance: self.0.required(instance)?,
            source_pin: self.0.required(source_pin)?,
            artifact: self.0.required(artifact)?,
            pin: self.0.required(pin)?,
        })
    }
}
struct Edge<'a>(&'a mut ParseState);
impl Element for PackageGraphEdge {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        input.deserialize_map(Edge(state))
    }
}
impl<'de> Visitor<'de> for Edge<'_> {
    type Value = PackageGraphEdge;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("real package dependency edge")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut from, mut to, mut declaration, mut seen) = (None, None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "from" => {
                    self.0.once(&mut seen, 1)?;
                    from = Some(map.next_value::<usize>()?);
                }
                "to" => {
                    self.0.once(&mut seen, 2)?;
                    to = Some(map.next_value::<usize>()?);
                }
                "declaration" => {
                    self.0.once(&mut seen, 4)?;
                    declaration = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 16 * 1024,
                    })?);
                }
                _ => return self.0.fail("unknown package edge field"),
            }
        }
        Ok(PackageGraphEdge {
            from: self.0.required(from)?,
            to: self.0.required(to)?,
            declaration: self.0.required(declaration)?,
        })
    }
}

struct Pin<'a>(&'a mut ParseState);
impl<'de> DeserializeSeed<'de> for Pin<'_> {
    type Value = PackageSourcePin;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Pin<'_> {
    type Value = PackageSourcePin;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("portable registry pin facts")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut registry, mut base, mut filename, mut size, mut advertised, mut seen) =
            (None, None, None, None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "registry" => {
                    self.0.once(&mut seen, 1)?;
                    registry = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 1024,
                    })?);
                }
                "base" => {
                    self.0.once(&mut seen, 2)?;
                    base = Some(map.next_value::<[u8; 32]>()?);
                }
                "filename" => {
                    self.0.once(&mut seen, 4)?;
                    filename = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 16 * 1024,
                    })?);
                }
                "size" => {
                    self.0.once(&mut seen, 8)?;
                    size = Some(map.next_value::<u64>()?);
                }
                "advertised" => {
                    self.0.once(&mut seen, 16)?;
                    advertised = Some(map.next_value::<bool>()?);
                }
                _ => return self.0.fail("unknown package source pin field"),
            }
        }
        Ok(PackageSourcePin {
            registry: self.0.required(registry)?,
            base: self.0.required(base)?,
            filename: self.0.required(filename)?,
            size: self.0.required(size)?,
            advertised: self.0.required(advertised)?,
        })
    }
}
