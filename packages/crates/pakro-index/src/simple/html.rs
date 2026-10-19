//! HTML Simple API from an explicitly decoded UTF-8 document.
//!
//! A4 separately bounds raw HTTP input, validates charset decoding and bounds
//! expansion. This API's common response limit counts decoded UTF-8 bytes.
//! The caller's logger must suppress html5ever's raw-token Debug/Trace targets.

use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;
use std::collections::BTreeMap;

use html5ever::TokenizerResult;
use html5ever::buffer_queue::BufferQueue;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use html5ever::tree_builder::{TreeBuilder, TreeBuilderOpts};
use reqwest::Url;

use super::facts::{self, RawCore, RawFile, RawMeta, RawProject, RawStatus};
use super::{
    SimpleError, SimpleLimitKind, SimpleParseContext, SimpleProject, SimpleResult, Yanked,
};

mod arena;
use arena::{ArenaSink, Handle, NodeData, TreeState};

#[derive(Debug, Clone, Copy)]
pub struct SimpleHtmlLimits {
    /// Visible tokenizer callbacks, including errors, text, comments and EOF.
    pub max_tokens: usize,
    /// All allocated arena nodes, including document/template fragments.
    pub max_tree_nodes: usize,
    pub max_tree_depth: usize,
    /// Cumulative text/comment/doctype bytes delivered to the tree sink.
    pub max_tree_text_bytes: usize,
    /// Arena edits, scanned siblings/ancestors and final projection visits.
    pub max_tree_operations: usize,
    /// Attributes surviving HTML5 tokenization's duplicate-name handling.
    pub max_attributes_per_tag: usize,
    /// Created HTML anchors, including subsequently inert/detached anchors.
    pub max_anchors: usize,
    /// UTF-8 feed size; one complete scalar may require up to four bytes.
    pub feed_chunk_bytes: usize,
}
impl Default for SimpleHtmlLimits {
    fn default() -> Self {
        Self {
            max_tokens: 4_000_000,
            max_tree_nodes: 1_048_576,
            max_tree_depth: 256,
            max_tree_text_bytes: 64 * 1024 * 1024,
            max_tree_operations: 16_000_000,
            max_attributes_per_tag: 128,
            max_anchors: 262_144,
            feed_chunk_bytes: 4096,
        }
    }
}

pub fn parse_simple_html(
    document: &str,
    context: SimpleParseContext<'_>,
    limits: SimpleHtmlLimits,
) -> SimpleResult<SimpleProject> {
    parse_inner(document, context, limits, None).result
}

pub(super) fn parse_for_document(
    text: &str,
    context: SimpleParseContext<'_>,
    limits: SimpleHtmlLimits,
    document: &super::client::http::EffectiveSimpleDocument,
) -> SimpleResult<SimpleProject> {
    parse_inner(text, context, limits, Some(document)).result
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct HtmlParseTrace {
    pub(super) fed_chunks: usize,
    pub(super) delegated_tokens: usize,
    pub(super) fed_chunks_at_error: Option<usize>,
    pub(super) delegated_tokens_at_error: Option<usize>,
    pub(super) ended_after_error: bool,
}
#[cfg(test)]
pub(super) fn parse_html_with_trace(
    document: &str,
    context: SimpleParseContext<'_>,
    limits: SimpleHtmlLimits,
) -> (SimpleResult<SimpleProject>, HtmlParseTrace) {
    let outcome = parse_inner(document, context, limits, None);
    (outcome.result, outcome.trace)
}
struct Outcome {
    result: SimpleResult<SimpleProject>,
    #[cfg(test)]
    trace: HtmlParseTrace,
}
impl Outcome {
    fn error(error: SimpleError) -> Self {
        Self {
            result: Err(error),
            #[cfg(test)]
            trace: HtmlParseTrace::default(),
        }
    }
}

struct BudgetedSink {
    tree: TreeBuilder<Handle, ArenaSink>,
    tokens: Cell<usize>,
    limits: SimpleHtmlLimits,
    #[cfg(test)]
    trace: RefCell<HtmlParseTrace>,
}
impl BudgetedSink {
    fn fail(&self, error: SimpleError) {
        let mut state = self.tree.sink.state.borrow_mut();
        if state.error.is_none() {
            state.error = Some(error);
        }
    }
    fn error(&self) -> Option<SimpleError> {
        self.tree.sink.state.borrow().error.clone()
    }
    fn note_error(&self) {
        #[cfg(test)]
        if self.error().is_some() {
            let mut trace = self.trace.borrow_mut();
            if trace.fed_chunks_at_error.is_none() {
                trace.fed_chunks_at_error = Some(trace.fed_chunks);
                trace.delegated_tokens_at_error = Some(trace.delegated_tokens);
            }
        }
    }
}
impl TokenSink for BudgetedSink {
    type Handle = Handle;
    fn process_token(&self, token: Token, line: u64) -> TokenSinkResult<Handle> {
        if self.error().is_some() {
            return TokenSinkResult::Continue;
        }
        let count = self.tokens.get().checked_add(1);
        match count {
            Some(count) if count <= self.limits.max_tokens => self.tokens.set(count),
            _ => {
                self.fail(SimpleError::LimitExceeded(SimpleLimitKind::HtmlTokens));
                self.note_error();
                return TokenSinkResult::Continue;
            }
        }
        if matches!(&token, Token::TagToken(tag) if tag.attrs.len()>self.limits.max_attributes_per_tag)
        {
            self.fail(SimpleError::LimitExceeded(SimpleLimitKind::HtmlAttributes));
            self.note_error();
            return TokenSinkResult::Continue;
        }
        #[cfg(test)]
        {
            self.trace.borrow_mut().delegated_tokens += 1;
        }
        // Every delegated token completes with a consistent tree. Budget errors
        // latch inside callbacks but never invent handles or skip half an edit.
        let result = self.tree.process_token(token, line);
        self.note_error();
        if self.error().is_some() {
            TokenSinkResult::Continue
        } else {
            result
        }
    }
    fn end(&self) {
        if self.error().is_none() {
            self.tree.end();
        }
    }
    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.error().is_none()
            && self
                .tree
                .adjusted_current_node_present_but_not_in_html_namespace()
    }
}

fn parse_inner(
    document: &str,
    context: SimpleParseContext<'_>,
    limits: SimpleHtmlLimits,
    effective: Option<&super::client::http::EffectiveSimpleDocument>,
) -> Outcome {
    let validation = (|| {
        facts::validate_limits(context.limits)?;
        if [
            limits.max_tokens,
            limits.max_tree_nodes,
            limits.max_tree_depth,
            limits.max_tree_text_bytes,
            limits.max_tree_operations,
            limits.max_attributes_per_tag,
            limits.max_anchors,
            limits.feed_chunk_bytes,
        ]
        .contains(&0)
        {
            return Err(SimpleError::InvalidLimits);
        }
        if document.len() as u64 > context.limits.max_response_bytes {
            return Err(SimpleError::LimitExceeded(SimpleLimitKind::ResponseBytes));
        }
        facts::validate_context(&context)
    })();
    if let Err(error) = validation {
        return Outcome::error(error);
    }
    let tree = TreeBuilder::new(
        ArenaSink::new(limits),
        TreeBuilderOpts {
            scripting_enabled: false,
            ..TreeBuilderOpts::default()
        },
    );
    let tokenizer = Tokenizer::new(
        BudgetedSink {
            tree,
            tokens: Cell::new(0),
            limits,
            #[cfg(test)]
            trace: RefCell::new(HtmlParseTrace::default()),
        },
        TokenizerOpts::default(),
    );
    let queue = BufferQueue::default();
    let mut position = 0usize;
    while position < document.len() {
        let mut end = position
            .saturating_add(limits.feed_chunk_bytes)
            .min(document.len());
        while !document.is_char_boundary(end) {
            end -= 1;
        }
        if end == position {
            end = position
                + document[position..]
                    .chars()
                    .next()
                    .expect("remaining UTF-8 scalar")
                    .len_utf8();
        }
        queue.push_back(StrTendril::from_slice(&document[position..end]));
        #[cfg(test)]
        {
            tokenizer.sink.trace.borrow_mut().fed_chunks += 1;
        }
        loop {
            let result = tokenizer.feed(&queue);
            if tokenizer.sink.error().is_some() || matches!(result, TokenizerResult::Done) {
                break;
            }
            // This static client neither executes a script nor changes the
            // caller's already-decoded character encoding. Resume the library.
        }
        if tokenizer.sink.error().is_some() {
            break;
        }
        position = end;
    }
    if tokenizer.sink.error().is_none() {
        #[cfg(test)]
        {
            tokenizer.sink.trace.borrow_mut().ended_after_error = tokenizer.sink.error().is_some();
        }
        tokenizer.end();
    }
    let error = tokenizer.sink.error();
    #[cfg(test)]
    let trace = tokenizer.sink.trace.into_inner();
    let mut state = tokenizer.sink.tree.sink.state.into_inner();
    let result = match error {
        Some(error) => Err(error),
        None => project(
            &mut state,
            &context,
            effective.map_or(context.canonical_document_url.as_url(), |value| {
                value.canonical()
            }),
        ),
    };
    Outcome {
        result,
        #[cfg(test)]
        trace,
    }
}

fn invalid(reason: &'static str) -> SimpleError {
    SimpleError::InvalidResponse(reason)
}
fn checked_text(value: &str, context: &SimpleParseContext<'_>) -> SimpleResult<String> {
    if value.len() > context.limits.max_field_bytes {
        return Err(SimpleError::LimitExceeded(SimpleLimitKind::FieldBytes));
    }
    Ok(value.to_owned())
}
fn attr<'a>(attrs: &'a [html5ever::Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|value| {
            AsRef::<str>::as_ref(&value.name.ns) == ""
                && AsRef::<str>::as_ref(&value.name.local) == name
        })
        .map(|value| value.value.as_ref())
}
fn single(slot: &mut Option<String>, value: String) -> SimpleResult<()> {
    if slot.as_ref().is_some_and(|old| old != &value) {
        return Err(invalid("conflicting HTML metadata"));
    }
    *slot = Some(value);
    Ok(())
}
fn anchor_text(
    state: &mut TreeState,
    root: Handle,
    context: &SimpleParseContext<'_>,
) -> SimpleResult<String> {
    let mut pending = state.nodes[root]
        .children
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    let mut text = String::new();
    while let Some(handle) = pending.pop() {
        state.charge(1);
        state.check()?;
        match &state.nodes[handle].data {
            NodeData::Text(value) => {
                let size = text
                    .len()
                    .checked_add(value.len())
                    .ok_or(SimpleError::LimitExceeded(SimpleLimitKind::FieldBytes))?;
                if size > context.limits.max_field_bytes {
                    return Err(SimpleError::LimitExceeded(SimpleLimitKind::FieldBytes));
                }
                text.push_str(value);
            }
            NodeData::Element {
                template: Some(_), ..
            } => continue,
            _ => {}
        }
        pending.extend(state.nodes[handle].children.iter().rev().copied());
    }
    Ok(text
        .trim_matches(|value: char| value.is_ascii_whitespace())
        .to_owned())
}
fn core(value: Option<&str>, context: &SimpleParseContext<'_>) -> SimpleResult<Option<RawCore>> {
    value
        .map(|value| {
            let value = checked_text(value, context)?;
            if value == "true" {
                return Ok(RawCore::Available(BTreeMap::new()));
            }
            let (name, digest) = value
                .split_once('=')
                .ok_or_else(|| invalid("invalid HTML core metadata"))?;
            let hashes = BTreeMap::from([(name.to_owned(), digest.to_owned())]);
            facts::normalize_hashes(hashes.clone())?;
            Ok(RawCore::Available(hashes))
        })
        .transpose()
}

fn project(
    state: &mut TreeState,
    context: &SimpleParseContext<'_>,
    document_base: &Url,
) -> SimpleResult<SimpleProject> {
    let mut raw = RawProject {
        name: Some(context.project.as_str().to_owned()),
        meta: Some(RawMeta::default()),
        files: Some(Vec::new()),
        ..RawProject::default()
    };
    let mut pending = vec![(0, false)];
    let mut anchors = Vec::new();
    let mut base = None;
    let mut relation_count = 0usize;
    while let Some((handle, in_head)) = pending.pop() {
        state.charge(1);
        state.check()?;
        let mut child_head = in_head;
        if let NodeData::Element {
            name,
            attrs,
            template,
            duplicate_attributes,
            ..
        } = &state.nodes[handle].data
        {
            if template.is_some() {
                continue;
            }
            if AsRef::<str>::as_ref(&name.ns) == "http://www.w3.org/1999/xhtml" {
                match AsRef::<str>::as_ref(&name.local) {
                    "head" => child_head = true,
                    "a" if attr(attrs, "href").is_some() => {
                        if *duplicate_attributes {
                            return Err(invalid("duplicate attributes on an HTML file link"));
                        }
                        if anchors.len() >= context.limits.max_files {
                            return Err(SimpleError::LimitExceeded(SimpleLimitKind::Files));
                        }
                        anchors.push(handle);
                    }
                    "base" if base.is_none() && attr(attrs, "href").is_some() => {
                        if *duplicate_attributes {
                            return Err(invalid("duplicate attributes on an HTML base"));
                        }
                        let value = checked_text(attr(attrs, "href").unwrap(), context)?;
                        let value = value.trim_matches(|value: char| value.is_ascii_whitespace());
                        let candidate = document_base.join(value);
                        base = Some(match candidate {
                            Err(_) => document_base.clone(),
                            Ok(candidate)
                                if matches!(candidate.scheme(), "data" | "javascript") =>
                            {
                                document_base.clone()
                            }
                            Ok(_) => facts::locator(value, document_base)?.url,
                        });
                    }
                    "meta" if in_head => {
                        if let Some(meta_name) = attr(attrs, "name") {
                            let meta_name = meta_name.to_ascii_lowercase();
                            if [
                                "pypi:repository-version",
                                "pypi:tracks",
                                "pypi:alternate-locations",
                                "pypi:project-status",
                                "pypi:project-status-reason",
                            ]
                            .contains(&meta_name.as_str())
                            {
                                if *duplicate_attributes {
                                    return Err(invalid("duplicate attributes on HTML metadata"));
                                }
                                let value = checked_text(
                                    attr(attrs, "content")
                                        .ok_or_else(|| invalid("missing HTML metadata content"))?,
                                    context,
                                )?;
                                match meta_name.as_str() {
                                    "pypi:repository-version" => {
                                        single(&mut raw.meta.as_mut().unwrap().api, value)?
                                    }
                                    "pypi:tracks" | "pypi:alternate-locations" => {
                                        if relation_count >= context.limits.max_relations {
                                            return Err(SimpleError::LimitExceeded(
                                                SimpleLimitKind::Relations,
                                            ));
                                        }
                                        relation_count += 1;
                                        if meta_name == "pypi:tracks" {
                                            raw.meta.as_mut().unwrap().tracks.push(value);
                                        } else {
                                            raw.alternate.push(value);
                                        }
                                    }
                                    "pypi:project-status" => single(
                                        &mut raw
                                            .status
                                            .get_or_insert_with(RawStatus::default)
                                            .status,
                                        value,
                                    )?,
                                    "pypi:project-status-reason" => single(
                                        &mut raw
                                            .status
                                            .get_or_insert_with(RawStatus::default)
                                            .reason,
                                        value,
                                    )?,
                                    _ => unreachable!(),
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        pending.extend(
            state.nodes[handle]
                .children
                .iter()
                .rev()
                .map(|value| (*value, child_head)),
        );
    }
    if raw.meta.as_ref().unwrap().api.is_none() {
        raw.meta.as_mut().unwrap().api = Some("1.0".to_owned());
    }
    let base = base.unwrap_or_else(|| document_base.clone());
    for handle in anchors {
        let filename = anchor_text(state, handle, context)?;
        let NodeData::Element { attrs, .. } = &state.nodes[handle].data else {
            unreachable!()
        };
        let raw_core = core(
            attr(attrs, "data-core-metadata").or_else(|| attr(attrs, "data-dist-info-metadata")),
            context,
        )?;
        let yanked = attr(attrs, "data-yanked")
            .map(|value| {
                checked_text(value, context).map(|value| Yanked::Yes {
                    reason: (!value.is_empty()).then_some(value),
                })
            })
            .transpose()?;
        let provenance = attr(attrs, "data-provenance")
            .map(|value| -> SimpleResult<String> {
                let value = checked_text(value, context)?;
                Url::parse(&value)
                    .map_err(|_| invalid("HTML provenance must be fully qualified"))?;
                Ok(value)
            })
            .transpose()?;
        raw.files.as_mut().unwrap().push(RawFile {
            filename: Some(filename),
            url: Some(checked_text(attr(attrs, "href").unwrap(), context)?),
            hashes: Some(BTreeMap::new()),
            requires_python: attr(attrs, "data-requires-python")
                .map(|value| checked_text(value, context))
                .transpose()?,
            yanked,
            core: raw_core,
            provenance,
            ..RawFile::default()
        });
    }
    facts::finish_html_project(raw, context, &base)
}
