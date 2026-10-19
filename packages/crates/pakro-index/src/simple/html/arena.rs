//! Private index arena implementing the complete tree-builder callbacks.

use std::borrow::Cow;
use std::cell::{Ref, RefCell};

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, QualName};

use super::SimpleHtmlLimits;
use crate::simple::{SimpleError, SimpleLimitKind, SimpleResult};

pub(super) type Handle = usize;

pub(super) enum NodeData {
    Document,
    Fragment,
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
        template: Option<Handle>,
        integration: bool,
        duplicate_attributes: bool,
    },
    Text(String),
    Other,
}

pub(super) struct Node {
    pub(super) parent: Option<Handle>,
    pub(super) children: Vec<Handle>,
    pub(super) data: NodeData,
}

pub(super) struct TreeState {
    pub(super) nodes: Vec<Node>,
    pub(super) error: Option<SimpleError>,
    limits: SimpleHtmlLimits,
    work: usize,
    text_bytes: usize,
    anchors: usize,
}

impl TreeState {
    fn fail(&mut self, error: SimpleError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }
    pub(super) fn charge(&mut self, amount: usize) {
        match self.work.checked_add(amount) {
            Some(value) => {
                self.work = value;
                if value > self.limits.max_tree_operations {
                    self.fail(SimpleError::LimitExceeded(
                        SimpleLimitKind::HtmlTreeOperations,
                    ));
                }
            }
            None => self.fail(SimpleError::LimitExceeded(
                SimpleLimitKind::HtmlTreeOperations,
            )),
        }
    }
    fn text(&mut self, amount: usize) {
        match self.text_bytes.checked_add(amount) {
            Some(value) => {
                self.text_bytes = value;
                if value > self.limits.max_tree_text_bytes {
                    self.fail(SimpleError::LimitExceeded(
                        SimpleLimitKind::HtmlTreeTextBytes,
                    ));
                }
            }
            None => self.fail(SimpleError::LimitExceeded(
                SimpleLimitKind::HtmlTreeTextBytes,
            )),
        }
    }
    fn create(&mut self, data: NodeData) -> Handle {
        self.charge(1);
        if matches!(&data, NodeData::Element { name, .. } if AsRef::<str>::as_ref(&name.ns) == "http://www.w3.org/1999/xhtml" && AsRef::<str>::as_ref(&name.local) == "a")
        {
            self.anchors = self.anchors.saturating_add(1);
            if self.anchors > self.limits.max_anchors {
                self.fail(SimpleError::LimitExceeded(SimpleLimitKind::HtmlAnchors));
            }
        }
        let handle = self.nodes.len();
        if handle >= self.limits.max_tree_nodes {
            self.fail(SimpleError::LimitExceeded(SimpleLimitKind::HtmlTreeNodes));
        }
        self.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            data,
        });
        handle
    }
    fn detach(&mut self, handle: Handle) {
        self.charge(1);
        if let Some(parent) = self.nodes[handle].parent.take() {
            if let Some(index) = self.nodes[parent]
                .children
                .iter()
                .position(|value| *value == handle)
            {
                self.charge(index.saturating_add(1));
                self.charge(self.nodes[parent].children.len().saturating_sub(index + 1));
                self.nodes[parent].children.remove(index);
            }
        }
    }
    fn observe_depth(&mut self, root: Handle) {
        let mut pending = vec![root];
        while let Some(handle) = pending.pop() {
            self.charge(1);
            let mut depth = 0usize;
            let mut parent = self.nodes[handle].parent;
            while let Some(handle) = parent {
                self.charge(1);
                depth = depth.saturating_add(1);
                if depth > self.nodes.len() {
                    self.fail(SimpleError::InvalidResponse("inconsistent HTML tree"));
                    break;
                }
                parent = self.nodes[handle].parent;
            }
            if depth > self.limits.max_tree_depth {
                self.fail(SimpleError::LimitExceeded(SimpleLimitKind::HtmlTreeDepth));
            }
            pending.extend(self.nodes[handle].children.iter().copied());
            if let NodeData::Element {
                template: Some(fragment),
                ..
            } = self.nodes[handle].data
            {
                pending.push(fragment);
            }
        }
    }
    fn insert(&mut self, parent: Handle, before: Option<Handle>, child: NodeOrText<Handle>) {
        self.charge(1);
        let index = if let Some(before) = before {
            let index = self.nodes[parent]
                .children
                .iter()
                .position(|value| *value == before)
                .expect("tree-builder sibling has no parent");
            self.charge(index.saturating_add(1));
            index
        } else {
            self.nodes[parent].children.len()
        };
        let handle = match child {
            NodeOrText::AppendNode(handle) => {
                self.detach(handle);
                handle
            }
            NodeOrText::AppendText(text) => {
                self.text(text.len());
                if index > 0 {
                    let previous = self.nodes[parent].children[index - 1];
                    if let NodeData::Text(value) = &mut self.nodes[previous].data {
                        value.push_str(&text);
                        return;
                    }
                }
                self.create(NodeData::Text(text.to_string()))
            }
        };
        // A moved child can have preceded the sibling in this same parent.
        let index = if let Some(before) = before {
            let index = self.nodes[parent]
                .children
                .iter()
                .position(|value| *value == before)
                .expect("tree-builder sibling disappeared");
            self.charge(index.saturating_add(1));
            index
        } else {
            self.nodes[parent].children.len()
        };
        self.nodes[handle].parent = Some(parent);
        self.charge(self.nodes[parent].children.len().saturating_sub(index));
        self.nodes[parent].children.insert(index, handle);
        self.observe_depth(handle);
    }
    pub(super) fn check(&self) -> SimpleResult<()> {
        self.error.clone().map_or(Ok(()), Err)
    }
}

pub(super) struct ArenaSink {
    pub(super) state: RefCell<TreeState>,
}
impl ArenaSink {
    pub(super) fn new(limits: SimpleHtmlLimits) -> Self {
        Self {
            state: RefCell::new(TreeState {
                nodes: vec![Node {
                    parent: None,
                    children: Vec::new(),
                    data: NodeData::Document,
                }],
                error: None,
                limits,
                work: 0,
                text_bytes: 0,
                anchors: 0,
            }),
        }
    }
}
impl TreeSink for ArenaSink {
    type Handle = Handle;
    type Output = TreeState;
    type ElemName<'a> = Ref<'a, QualName>;
    fn finish(self) -> Self::Output {
        self.state.into_inner()
    }
    fn parse_error(&self, _message: Cow<'static, str>) {}
    fn get_document(&self) -> Handle {
        0
    }
    fn elem_name<'a>(&'a self, target: &'a Handle) -> Self::ElemName<'a> {
        Ref::map(self.state.borrow(), |state| {
            match &state.nodes[*target].data {
                NodeData::Element { name, .. } => name,
                _ => panic!("tree-builder requested the name of a non-element"),
            }
        })
    }
    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        let mut state = self.state.borrow_mut();
        let fragment = flags.template.then(|| state.create(NodeData::Fragment));
        let handle = state.create(NodeData::Element {
            name,
            attrs,
            template: fragment,
            integration: flags.mathml_annotation_xml_integration_point,
            duplicate_attributes: flags.had_duplicate_attributes,
        });
        if let Some(fragment) = fragment {
            state.nodes[fragment].parent = Some(handle);
        }
        handle
    }
    fn create_comment(&self, text: StrTendril) -> Handle {
        let mut state = self.state.borrow_mut();
        state.text(text.len());
        state.create(NodeData::Other)
    }
    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Handle {
        let mut state = self.state.borrow_mut();
        state.text(target.len().saturating_add(data.len()));
        state.create(NodeData::Other)
    }
    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        self.state.borrow_mut().insert(*parent, None, child);
    }
    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        previous: &Handle,
        child: NodeOrText<Handle>,
    ) {
        let parent = self.state.borrow().nodes[*element].parent;
        if parent.is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(previous, child);
        }
    }
    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let mut state = self.state.borrow_mut();
        state.text(
            name.len()
                .saturating_add(public_id.len())
                .saturating_add(system_id.len()),
        );
        let node = state.create(NodeData::Other);
        state.insert(0, None, NodeOrText::AppendNode(node));
    }
    fn get_template_contents(&self, target: &Handle) -> Handle {
        match &self.state.borrow().nodes[*target].data {
            NodeData::Element {
                template: Some(fragment),
                ..
            } => *fragment,
            _ => panic!("tree-builder requested missing template contents"),
        }
    }
    fn same_node(&self, left: &Handle, right: &Handle) -> bool {
        left == right
    }
    fn set_quirks_mode(&self, _mode: QuirksMode) {}
    fn append_before_sibling(&self, sibling: &Handle, child: NodeOrText<Handle>) {
        let mut state = self.state.borrow_mut();
        let parent = state.nodes[*sibling]
            .parent
            .expect("tree-builder sibling has no parent");
        state.insert(parent, Some(*sibling), child);
    }
    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        let mut state = self.state.borrow_mut();
        for attr in attrs {
            let NodeData::Element { attrs, .. } = &mut state.nodes[*target].data else {
                unreachable!("tree-builder attribute target is not an element")
            };
            let scanned = attrs.len();
            if !attrs.iter().any(|old| old.name == attr.name) {
                attrs.push(attr);
            }
            state.charge(scanned.saturating_add(1));
        }
    }
    fn remove_from_parent(&self, target: &Handle) {
        self.state.borrow_mut().detach(*target);
    }
    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let mut state = self.state.borrow_mut();
        let children = std::mem::take(&mut state.nodes[*node].children);
        for child in children {
            state.nodes[child].parent = None;
            state.insert(*new_parent, None, NodeOrText::AppendNode(child));
        }
    }
    fn is_mathml_annotation_xml_integration_point(&self, target: &Handle) -> bool {
        matches!(
            &self.state.borrow().nodes[*target].data,
            NodeData::Element {
                integration: true,
                ..
            }
        )
    }
    fn allow_declarative_shadow_roots(&self, _parent: &Handle) -> bool {
        false
    }
}
