// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronDOM — arena-allocated document tree.
//!
//! Nodes live in a `Vec` and are addressed by `NodeId` (an index). Ids are
//! never reused within a document, so a stale id held by script can at worst
//! point at a detached node, never at an unrelated one.

use url::Url;

pub type NodeId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    Html,
    Svg,
    MathMl,
    /// Any other namespace (URI kept in `Document::ns_uris`; empty = null namespace).
    Other,
}

#[derive(Clone, Debug)]
pub struct Attr {
    /// Qualified name (`prefix:local` or `local`).
    pub name: String,
    pub value: String,
    /// Namespace URI; None is the null namespace.
    pub ns: Option<Box<str>>,
}

pub const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
pub const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

impl Attr {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Attr {
        Attr { name: name.into(), value: value.into(), ns: None }
    }
    pub fn local_name(&self) -> &str {
        match self.name.find(':') {
            Some(i) if self.ns.is_some() => &self.name[i + 1..],
            _ => &self.name,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ElementData {
    pub name: String,
    pub ns: Namespace,
    pub attrs: Vec<Attr>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    /// Live value of form controls (input/textarea/select) once it diverges
    /// from the markup.
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub selected: Option<bool>,
    /// The `indeterminate` IDL flag of checkboxes (script-only state).
    pub indeterminate: bool,
    /// `setCustomValidity()` message.
    pub custom_validity: Option<String>,
    /// Popover showing state (`showPopover()`).
    pub popover_open: bool,
    /// A dialog opened with showModal() (matches `:modal`).
    pub modal: bool,
    /// A custom element that has been constructed (matches `:defined`).
    pub ce_defined: bool,
}

impl ElementData {
    pub fn new(name: &str, ns: Namespace, attrs: Vec<Attr>) -> ElementData {
        let mut e = ElementData {
            name: name.to_string(),
            ns,
            attrs,
            id: None,
            classes: Vec::new(),
            value: None,
            checked: None,
            selected: None,
            indeterminate: false,
            custom_validity: None,
            popover_open: false,
            modal: false,
            ce_defined: false,
        };
        e.refresh_cache();
        e
    }

    fn refresh_cache(&mut self) {
        // Only the null-namespace id/class attributes count.
        let plain = |n: &str| self.attrs.iter().find(|a| a.ns.is_none() && a.name == n).map(|a| a.value.as_str());
        self.id = plain("id").filter(|s| !s.is_empty()).map(|s| s.to_string());
        self.classes = plain("class")
            .map(|c| c.split_ascii_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default();
    }

    /// Attribute value; names match exactly, else ignoring ASCII case (SVG keeps
    /// camelCase names like `viewBox` that lowercase lookups must still find).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name == name)
            .or_else(|| if self.ns == Namespace::Html { None } else { self.attrs.iter().find(|a| a.name.eq_ignore_ascii_case(name)) })
            .map(|a| a.value.as_str())
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attr(name).is_some()
    }

    /// The first attribute whose qualified name is exactly `name`.
    pub fn attr_exact(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.name == name).map(|a| a.value.as_str())
    }

    pub fn set_attr(&mut self, name: &str, value: &str) {
        if let Some(a) = self.attrs.iter_mut().find(|a| a.name == name) {
            a.value = value.to_string();
        } else {
            self.attrs.push(Attr::new(name, value));
        }
        if name == "id" || name == "class" {
            self.refresh_cache();
        }
    }

    /// Index of the attribute with namespace `ns` and local name `local`.
    pub fn attr_index_ns(&self, ns: Option<&str>, local: &str) -> Option<usize> {
        self.attrs.iter().position(|a| a.ns.as_deref() == ns && a.local_name() == local)
    }

    /// setAttributeNS: change the value of the (ns, local) attribute (keeping
    /// its prefix) or append a new one. Returns the old value.
    pub fn set_attr_ns(&mut self, ns: Option<&str>, qname: &str, value: &str) -> Option<String> {
        let local = match qname.find(':') {
            Some(i) if ns.is_some() => &qname[i + 1..],
            _ => qname,
        };
        let old = if let Some(i) = self.attr_index_ns(ns, local) {
            Some(std::mem::replace(&mut self.attrs[i].value, value.to_string()))
        } else {
            self.attrs.push(Attr { name: qname.to_string(), value: value.to_string(), ns: ns.map(|n| n.into()) });
            None
        };
        if local == "id" || local == "class" {
            self.refresh_cache();
        }
        old
    }

    /// removeAttributeNS; returns the removed (qualified name, value).
    pub fn remove_attr_ns(&mut self, ns: Option<&str>, local: &str) -> Option<(String, String)> {
        let i = self.attr_index_ns(ns, local)?;
        let a = self.attrs.remove(i);
        if local == "id" || local == "class" {
            self.refresh_cache();
        }
        Some((a.name, a.value))
    }

    pub fn remove_attr(&mut self, name: &str) -> bool {
        let before = self.attrs.len();
        if let Some(i) = self.attrs.iter().position(|a| a.name == name) {
            self.attrs.remove(i);
        }
        if name == "id" || name == "class" {
            self.refresh_cache();
        }
        before != self.attrs.len()
    }

    pub fn has_class(&self, c: &str) -> bool {
        self.classes.iter().any(|x| x == c)
    }
}

#[derive(Clone, Debug)]
pub enum NodeData {
    Document,
    Doctype(String),
    Element(ElementData),
    Text(String),
    Comment(String),
    Fragment,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
    /// Owner document node (DOCUMENT for the main document; another Document
    /// node for iframe/created documents that live in the same arena).
    pub owner: NodeId,
}

pub struct Document {
    pub nodes: Vec<Node>,
    pub url: Url,
    pub base_url: Url,
    /// Bumped on every mutation; consumers compare to decide when to restyle.
    pub generation: u64,
    pub quirks: bool,
    /// Parsed as XML (XHTML and other XML documents): not an HTML document.
    pub is_xml: bool,
    /// The document's encoding: URL queries of the document are encoded in it.
    pub charset: &'static str,
    /// The fallback base URL (the document's URL unless set, e.g. a srcdoc
    /// document's parent's); <base href> resolves against it.
    pub fallback_base: Option<Url>,
    base_cache: std::cell::RefCell<Option<(u64, Url)>>,
    /// "Almost standards" mode (XHTML 1.0 Transitional/Frameset doctypes…).
    pub limited_quirks: bool,
    /// Custom element names registered with customElements.define (for :defined).
    pub defined_custom: std::collections::HashSet<String>,
    pub focused: Option<NodeId>,
    pub hovered: Option<NodeId>,
    pub title_override: Option<String>,
    /// Namespace URIs of `Namespace::Other` elements.
    pub ns_uris: std::collections::HashMap<NodeId, String>,
    /// Sub-document node -> the <iframe> element it belongs to.
    pub frame_of: std::collections::HashMap<NodeId, NodeId>,
    /// <template> element -> its contents fragment (kept out of the tree).
    pub template_contents: std::collections::HashMap<NodeId, NodeId>,
    /// Shadow host -> its shadow root (a fragment node with no parent).
    pub shadow_roots: std::collections::HashMap<NodeId, NodeId>,
    /// <script> elements that changed document (a frame's pending scripts
    /// moved elsewhere don't run).
    pub moved_scripts: std::collections::HashSet<NodeId>,
    /// Shadow root -> its host and options.
    pub shadow_info: std::collections::HashMap<NodeId, ShadowInfo>,
    /// Slots of shadow roots in manual slot-assignment mode -> nodes given to `assign()`.
    pub manual_slots: std::collections::HashMap<NodeId, Vec<NodeId>>,
    /// Constructed style sheets adopted by the document (DOCUMENT) or a shadow root.
    pub adopted: std::collections::HashMap<NodeId, Vec<std::rc::Rc<str>>>,
    slot_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<SlotMap>)>>,
    id_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<IdIndex>)>>,
    /// Style invalidation records of mutations since the last restyle.
    pub style_dirty: Vec<StyleRecord>,
    /// Generation bumps accounted for by `style_dirty`; a bump made anywhere
    /// else leaves this behind `generation` and forces a full restyle.
    pub style_bumps: u64,
    /// Text nodes that are CDATA sections (XML documents).
    pub cdata: std::collections::HashSet<NodeId>,
    /// Comment-backed processing instructions -> their target.
    pub pi_target: std::collections::HashMap<NodeId, String>,
    /// The top layer (modal dialogs, open popovers, fullscreen), bottom first.
    pub top_layer: Vec<NodeId>,
    /// Documents of local frames (sub-documents of this arena) -> the
    /// generation of their last mutation (to know when to render them again).
    pub sub_gens: std::collections::HashMap<NodeId, u64>,
    /// Quirks mode of sub-documents parsed into this arena.
    pub sub_quirks: std::collections::HashMap<NodeId, bool>,
    /// The document being styled/laid out/painted: DOCUMENT, or a local
    /// frame's sub-document while it renders (document_element(), body() and
    /// base URL resolution follow it).
    pub render_root: std::cell::Cell<NodeId>,
    /// (parent, index) of the last sibling-index lookup (see index_in_parent).
    sib_hint: std::cell::Cell<(NodeId, usize)>,
    /// Bumped by mutations of the tree's structure (not attributes or text):
    /// child lists are valid while it is unchanged.
    pub tree_gen: u64,
    /// The base URL while a sub-document renders.
    pub render_base: std::cell::RefCell<Option<Url>>,
    sub_id_cache: std::cell::RefCell<Option<(NodeId, u64, std::rc::Rc<IdIndex>)>>,
}

/// A mutation that may change styles (consumed by incremental restyles).
#[derive(Clone, Debug, PartialEq)]
pub enum StyleRecord {
    /// The node's child list changed (or its `:empty` state).
    Children(NodeId),
    /// A node was inserted into the parent's child list: (parent, child).
    Inserted(NodeId, NodeId),
    /// A child was removed: (parent, the node that followed it).
    Removed(NodeId, Option<NodeId>),
    /// An attribute changed: name, old value, new value.
    Attr(NodeId, String, Option<String>, Option<String>),
    /// Selector-visible element state changed (e.g. `:defined`).
    State(NodeId),
    /// A text node's data changed (no style effect; layout must be redone).
    Text(NodeId),
}

/// Elements of the document tree by id (first in tree order) and the
/// window's named properties (ids, plus `name` of embed/form/iframe/img/object).
#[derive(Default, Debug)]
pub struct IdIndex {
    pub by_id: std::collections::HashMap<String, NodeId>,
    pub named: std::collections::HashMap<String, Vec<NodeId>>,
}

/// A shadow root's host and the options it was attached with.
#[derive(Clone, Debug)]
pub struct ShadowInfo {
    pub host: NodeId,
    pub open: bool,
    pub delegates_focus: bool,
    pub manual: bool,
    pub clonable: bool,
    pub serializable: bool,
    /// Created by a declarative `<template shadowrootmode>`.
    pub declarative: bool,
}

/// Slot assignment of every shadow tree: which nodes each slot is given and
/// the slot each assigned node renders in.
#[derive(Default, Debug)]
pub struct SlotMap {
    pub assigned: std::collections::HashMap<NodeId, Vec<NodeId>>,
    pub slot_of: std::collections::HashMap<NodeId, NodeId>,
}

pub const DOCUMENT: NodeId = 0;

pub const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
    "basefont", "bgsound", "frame", "keygen",
];

pub fn is_void(name: &str) -> bool {
    VOID_ELEMENTS.contains(&name)
}

impl Document {
    pub fn new(url: Url) -> Document {
        Document {
            nodes: vec![Node { parent: None, children: Vec::new(), data: NodeData::Document, owner: DOCUMENT }],
            base_url: url.clone(),
            url,
            generation: 0,
            quirks: false,
            is_xml: false,
            charset: "UTF-8",
            fallback_base: None,
            base_cache: Default::default(),
            limited_quirks: false,
            defined_custom: std::collections::HashSet::new(),
            focused: None,
            hovered: None,
            title_override: None,
            ns_uris: std::collections::HashMap::new(),
            frame_of: std::collections::HashMap::new(),
            template_contents: std::collections::HashMap::new(),
            shadow_roots: std::collections::HashMap::new(),
            moved_scripts: std::collections::HashSet::new(),
            shadow_info: std::collections::HashMap::new(),
            manual_slots: std::collections::HashMap::new(),
            adopted: std::collections::HashMap::new(),
            slot_cache: std::cell::RefCell::new(None),
            id_cache: std::cell::RefCell::new(None),
            style_dirty: Vec::new(),
            style_bumps: 0,
            cdata: Default::default(),
            pi_target: Default::default(),
            top_layer: Vec::new(),
            sub_gens: Default::default(),
            sub_quirks: Default::default(),
            render_root: std::cell::Cell::new(DOCUMENT),
            sib_hint: std::cell::Cell::new((usize::MAX, 0)),
            tree_gen: 0,
            render_base: std::cell::RefCell::new(None),
            sub_id_cache: std::cell::RefCell::new(None),
        }
    }

    /// Bump the generation for a mutation and record what it may restyle.
    pub fn touch(&mut self, rec: StyleRecord) {
        self.generation += 1;
        if !matches!(rec, StyleRecord::Attr(..) | StyleRecord::State(_) | StyleRecord::Text(_)) {
            self.tree_gen += 1;
        }
        let node = match &rec {
            StyleRecord::Children(n) | StyleRecord::Inserted(n, _) | StyleRecord::Removed(n, _) | StyleRecord::Attr(n, ..) | StyleRecord::State(n) | StyleRecord::Text(n) => *n,
        };
        if !self.sub_gens.is_empty() {
            let d = self.tree_document(node);
            let g = self.generation;
            if let Some(x) = self.sub_gens.get_mut(&d) {
                *x = g;
            }
        }
        if self.style_dirty.len() >= 4096 {
            return; // too many: unaccounted, so the next restyle is a full one
        }
        self.style_bumps += 1;
        if self.style_dirty.last() == Some(&rec) || !self.style_connected(node) {
            return;
        }
        self.style_dirty.push(rec);
    }

    /// A mutation that cannot affect styles (still bumps the generation).
    fn touch_inert(&mut self, node: NodeId) {
        self.style_bumps += 1;
        self.bump_node(node);
    }

    /// A form control's value / checkedness / selectedness changed: restyle
    /// it (:placeholder-shown, :checked, :invalid, ...) and the forms and
    /// fieldsets around it (:invalid / :valid on them).
    pub fn control_changed(&mut self, node: NodeId) {
        self.touch(StyleRecord::State(node));
        let mut a = self.nodes.get(node).and_then(|n| n.parent);
        let mut guard = 0;
        while let Some(x) = a {
            guard += 1;
            if guard > 4096 {
                break;
            }
            if self.is_tag(x, "form") || self.is_tag(x, "fieldset") || self.is_tag(x, "select") {
                self.touch(StyleRecord::State(x));
            }
            a = self.nodes[x].parent;
        }
    }

    /// A change on `node` not tracked for styles: renders again only the
    /// document `node` is in (not every local frame).
    pub fn bump_node(&mut self, node: NodeId) {
        self.generation += 1;
        self.tree_gen += 1;
        if !self.sub_gens.is_empty() {
            let d = self.tree_document(node);
            let g = self.generation;
            if let Some(x) = self.sub_gens.get_mut(&d) {
                *x = g;
            }
        }
    }

    /// A change anywhere (not tracked to a node): every document renders again.
    pub fn bump_all(&mut self) {
        self.generation += 1;
        self.tree_gen += 1;
        self.bump_subdocs();
    }

    fn bump_subdocs(&mut self) {
        let g = self.generation;
        for v in self.sub_gens.values_mut() {
            *v = g;
        }
    }

    /// The document node whose tree `node` is in (through shadow hosts);
    /// a detached subtree's root otherwise.
    pub fn tree_document(&self, mut node: NodeId) -> NodeId {
        for _ in 0..4096 {
            match self.nodes[node].parent {
                Some(p) => node = p,
                None => match self.shadow_info.get(&node) {
                    Some(i) => node = i.host,
                    None => return node,
                },
            }
        }
        node
    }

    /// Is `node` in a tree that gets styled (a document, or a shadow tree of a
    /// connected host)? Detached subtrees need no invalidation.
    fn style_connected(&self, mut node: NodeId) -> bool {
        for _ in 0..4096 {
            match self.nodes[node].parent {
                Some(p) => node = p,
                None => {
                    if matches!(self.nodes[node].data, NodeData::Document) {
                        return true;
                    }
                    match self.shadow_info.get(&node) {
                        Some(i) => node = i.host,
                        None => return false,
                    }
                }
            }
        }
        true
    }

    /// Id / named-property index of the document tree (cached until the DOM changes).
    pub fn id_index(&self) -> std::rc::Rc<IdIndex> {
        if let Some((g, m)) = &*self.id_cache.borrow() {
            if *g == self.generation {
                return m.clone();
            }
        }
        let m = std::rc::Rc::new(self.build_id_index(DOCUMENT));
        *self.id_cache.borrow_mut() = Some((self.generation, m.clone()));
        m
    }

    /// The same for a sub-document's tree (a local frame's document).
    pub fn id_index_of(&self, root: NodeId) -> std::rc::Rc<IdIndex> {
        if root == DOCUMENT {
            return self.id_index();
        }
        let g = self.sub_gens.get(&root).copied().unwrap_or(self.generation);
        if let Some((r, gg, m)) = &*self.sub_id_cache.borrow() {
            if *r == root && *gg == g && self.sub_gens.contains_key(&root) {
                return m.clone();
            }
        }
        let m = std::rc::Rc::new(self.build_id_index(root));
        *self.sub_id_cache.borrow_mut() = Some((root, g, m.clone()));
        m
    }

    fn build_id_index(&self, root: NodeId) -> IdIndex {
        let mut idx = IdIndex::default();
        for n in self.descendants(root) {
            if let NodeData::Element(e) = &self.nodes[n].data {
                if let Some(id) = &e.id {
                    idx.by_id.entry(id.clone()).or_insert(n);
                    idx.named.entry(id.clone()).or_default().push(n);
                }
                if e.ns == Namespace::Html && matches!(e.name.as_str(), "embed" | "form" | "iframe" | "img" | "object") {
                    if let Some(nm) = e.attr("name").filter(|v| !v.is_empty()) {
                        let v = idx.named.entry(nm.to_string()).or_default();
                        if !v.contains(&n) {
                            v.push(n);
                        }
                    }
                }
            }
        }
        idx
    }

    // ------------------------------------------------------------- shadow DOM

    /// Attach a shadow root to `host`; None if it already has one.
    pub fn attach_shadow(&mut self, host: NodeId, info: ShadowInfo) -> Option<NodeId> {
        if self.shadow_roots.contains_key(&host) || !self.is_element(host) {
            return None;
        }
        let root = self.create_fragment();
        self.nodes[root].owner = self.owner_doc(host);
        self.shadow_roots.insert(host, root);
        self.shadow_info.insert(root, ShadowInfo { host, ..info });
        self.touch(StyleRecord::Children(host));
        Some(root)
    }

    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        self.shadow_roots.get(&host).copied()
    }

    pub fn is_shadow_root(&self, id: NodeId) -> bool {
        self.shadow_info.contains_key(&id)
    }

    /// The host of shadow root `root`.
    pub fn host_of(&self, root: NodeId) -> Option<NodeId> {
        self.shadow_info.get(&root).map(|i| i.host)
    }

    /// The shadow root whose tree contains `id` (None in a document or detached tree).
    pub fn containing_shadow_root(&self, id: NodeId) -> Option<NodeId> {
        let r = self.root_of(id);
        if self.is_shadow_root(r) {
            Some(r)
        } else {
            None
        }
    }

    /// Topmost ancestor, crossing from shadow roots to their hosts.
    pub fn shadow_including_root(&self, mut id: NodeId) -> NodeId {
        for _ in 0..1000 {
            let r = self.root_of(id);
            match self.host_of(r) {
                Some(h) => id = h,
                None => return r,
            }
        }
        id
    }

    /// Is `ancestor` a shadow-including inclusive ancestor of `node`?
    pub fn shadow_including_contains(&self, ancestor: NodeId, mut node: NodeId) -> bool {
        for _ in 0..100_000 {
            if node == ancestor {
                return true;
            }
            match self.parent(node).or_else(|| self.host_of(node)) {
                Some(p) => node = p,
                None => return false,
            }
        }
        false
    }

    /// Slot assignment of all shadow trees (cached until the DOM changes).
    pub fn slot_map(&self) -> std::rc::Rc<SlotMap> {
        if let Some((g, m)) = &*self.slot_cache.borrow() {
            if *g == self.generation {
                return m.clone();
            }
        }
        let mut map = SlotMap::default();
        if !self.shadow_roots.is_empty() {
            let mut hosts: Vec<(NodeId, NodeId)> = self.shadow_roots.iter().map(|(&h, &r)| (h, r)).collect();
            hosts.sort_unstable();
            for (host, root) in hosts {
                let manual = self.shadow_info.get(&root).map(|i| i.manual).unwrap_or(false);
                let slots: Vec<NodeId> = self.descendants(root).filter(|&d| self.is_tag(d, "slot")).collect();
                if slots.is_empty() {
                    continue;
                }
                if manual {
                    for &s in &slots {
                        let Some(given) = self.manual_slots.get(&s) else { continue };
                        let mut v = Vec::new();
                        for &n in given {
                            if self.parent(n) == Some(host) && !map.slot_of.contains_key(&n) {
                                map.slot_of.insert(n, s);
                                v.push(n);
                            }
                        }
                        if !v.is_empty() {
                            map.assigned.insert(s, v);
                        }
                    }
                    continue;
                }
                let mut by_name: std::collections::HashMap<&str, NodeId> = std::collections::HashMap::new();
                for &s in &slots {
                    by_name.entry(self.attr(s, "name").unwrap_or("")).or_insert(s);
                }
                for &c in &self.nodes[host].children {
                    let name = match &self.nodes[c].data {
                        NodeData::Element(e) => e.attr("slot").unwrap_or(""),
                        NodeData::Text(_) => "",
                        _ => continue,
                    };
                    if let Some(&s) = by_name.get(name) {
                        map.slot_of.insert(c, s);
                        map.assigned.entry(s).or_default().push(c);
                    }
                }
            }
        }
        let m = std::rc::Rc::new(map);
        *self.slot_cache.borrow_mut() = Some((self.generation, m.clone()));
        m
    }

    /// The slot `id` is assigned to.
    pub fn assigned_slot(&self, id: NodeId) -> Option<NodeId> {
        if self.shadow_roots.is_empty() {
            return None;
        }
        let p = self.parent(id)?;
        if !self.shadow_roots.contains_key(&p) {
            return None;
        }
        self.slot_map().slot_of.get(&id).copied()
    }

    /// Children in the flat tree: a host's shadow tree, a slot's assigned
    /// nodes (or its fallback content), otherwise the DOM children.
    pub fn flat_children(&self, id: NodeId) -> std::borrow::Cow<'_, [NodeId]> {
        if self.shadow_roots.is_empty() {
            return std::borrow::Cow::Borrowed(&self.nodes[id].children);
        }
        if let Some(&r) = self.shadow_roots.get(&id) {
            return std::borrow::Cow::Borrowed(&self.nodes[r].children);
        }
        if self.is_tag(id, "slot") && self.containing_shadow_root(id).is_some() {
            if let Some(v) = self.slot_map().assigned.get(&id) {
                return std::borrow::Cow::Owned(v.clone());
            }
        }
        std::borrow::Cow::Borrowed(&self.nodes[id].children)
    }

    /// Parent in the flat tree (None for nodes that are not rendered: children
    /// of a shadow host that no slot takes).
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        if self.shadow_roots.is_empty() {
            return self.parent(id);
        }
        let p = self.parent(id)?;
        if self.shadow_roots.contains_key(&p) {
            return self.slot_map().slot_of.get(&id).copied();
        }
        if let Some(h) = self.host_of(p) {
            return Some(h);
        }
        Some(p)
    }

    /// Nearest element ancestor in the flat tree.
    pub fn flat_parent_element(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.flat_parent(id)?;
        for _ in 0..10_000 {
            if self.is_element(cur) {
                return Some(cur);
            }
            cur = self.flat_parent(cur)?;
        }
        None
    }

    /// Declarative shadow DOM: each `<template shadowrootmode>` (open/closed)
    /// whose parent element has no shadow root yet becomes that parent's shadow
    /// root, taking the template's contents. Run after templates are hoisted.
    pub fn attach_declarative_shadows(&mut self, root: NodeId) {
        let mut todo = vec![root];
        while let Some(r) = todo.pop() {
            let temps: Vec<NodeId> = self
                .descendants(r)
                .filter(|&d| self.element(d).map(|e| e.name == "template" && e.ns == Namespace::Html && e.has_attr("shadowrootmode")).unwrap_or(false))
                .collect();
            for t in temps {
                let mode = self.attr(t, "shadowrootmode").unwrap_or("").to_ascii_lowercase();
                if mode != "open" && mode != "closed" {
                    continue;
                }
                let Some(host) = self.parent_element(t) else { continue };
                if self.shadow_roots.contains_key(&host) {
                    continue;
                }
                let info = ShadowInfo {
                    host,
                    open: mode == "open",
                    delegates_focus: self.has_attr(t, "shadowrootdelegatesfocus"),
                    manual: self.attr(t, "shadowrootslotassignment").map(|v| v.eq_ignore_ascii_case("manual")).unwrap_or(false),
                    clonable: self.has_attr(t, "shadowrootclonable"),
                    serializable: self.has_attr(t, "shadowrootserializable"),
                    declarative: true,
                };
                let Some(sr) = self.attach_shadow(host, info) else { continue };
                let content = self.template_content(t);
                let kids = self.nodes[content].children.clone();
                for k in kids {
                    self.append_child(sr, k);
                }
                self.detach(t);
                todo.push(sr);
            }
        }
    }

    /// Pre-order traversal that also enters shadow trees (each host's shadow
    /// root comes right after the host, before its light children).
    pub fn shadow_including_descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.nodes[root].children.iter().rev().copied().collect();
        if let Some(&r) = self.shadow_roots.get(&root) {
            stack.push(r);
        }
        while let Some(n) = stack.pop() {
            out.push(n);
            for &c in self.nodes[n].children.iter().rev() {
                stack.push(c);
            }
            if let Some(&r) = self.shadow_roots.get(&n) {
                stack.push(r);
            }
        }
        out
    }

    fn push(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node { parent: None, children: Vec::new(), data, owner: DOCUMENT });
        self.nodes.len() - 1
    }

    /// A new, empty document (for iframes / DOMImplementation) in this arena.
    pub fn create_document_node(&mut self) -> NodeId {
        let id = self.push(NodeData::Document);
        self.nodes[id].owner = id;
        id
    }

    pub fn is_document(&self, id: NodeId) -> bool {
        matches!(self.nodes.get(id).map(|n| &n.data), Some(NodeData::Document))
    }

    /// Topmost ancestor (or the node itself).
    pub fn root_of(&self, mut id: NodeId) -> NodeId {
        for _ in 0..100_000 {
            match self.nodes[id].parent {
                Some(p) => id = p,
                None => break,
            }
        }
        id
    }

    /// The document a node belongs to (a document is its own owner).
    pub fn owner_doc(&self, id: NodeId) -> NodeId {
        if self.is_document(id) {
            id
        } else {
            self.nodes.get(id).map(|n| n.owner).unwrap_or(DOCUMENT)
        }
    }

    /// Adopt a subtree into `owner`.
    pub fn set_owner_tree(&mut self, id: NodeId, owner: NodeId) {
        if self.nodes[id].owner == owner && !self.is_document(id) {
            // Fast path: assume descendants already agree.
            let first = self.nodes[id].children.first().copied();
            if first.map(|c| self.nodes[c].owner == owner).unwrap_or(true) {
                return;
            }
        }
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if !self.is_document(n) {
                if self.nodes[n].owner != owner {
                    if let NodeData::Element(e) = &self.nodes[n].data {
                        if e.name == "script" {
                            self.moved_scripts.insert(n);
                        }
                    }
                }
                self.nodes[n].owner = owner;
            }
            stack.extend(self.nodes[n].children.iter().copied());
        }
    }

    /// Deep-copy a node from another document into this arena (owned by `owner`).
    pub fn import_from(&mut self, other: &Document, src: NodeId, owner: NodeId) -> NodeId {
        let data = match &other.nodes[src].data {
            NodeData::Document => NodeData::Fragment,
            d => d.clone(),
        };
        let new = self.push(data);
        self.nodes[new].owner = owner;
        if let Some(u) = other.ns_uris.get(&src) {
            self.ns_uris.insert(new, u.clone());
        }
        if other.cdata.contains(&src) {
            self.cdata.insert(new);
        }
        if let Some(t) = other.pi_target.get(&src) {
            self.pi_target.insert(new, t.clone());
        }
        let kids = other.nodes[src].children.clone();
        for k in kids {
            let c = self.import_from(other, k, owner);
            self.nodes[c].parent = Some(new);
            self.nodes[new].children.push(c);
        }
        // A template's contents come along (they live outside the tree).
        if let Some(&f) = other.template_contents.get(&src) {
            // Owned like a fresh contents fragment (an inert document, not
            // the template's own one).
            let cf = self.import_from(other, f, DOCUMENT);
            self.template_contents.insert(new, cf);
        }
        new
    }

    pub fn get_element_by_id_in(&self, root: NodeId, id: &str) -> Option<NodeId> {
        self.descendants(root)
            .find(|&n| self.element(n).map(|e| e.id.as_deref() == Some(id)).unwrap_or(false))
    }

    pub fn create_element(&mut self, name: &str, ns: Namespace, attrs: Vec<Attr>) -> NodeId {
        self.push(NodeData::Element(ElementData::new(name, ns, attrs)))
    }

    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.push(NodeData::Text(text.to_string()))
    }

    pub fn create_comment(&mut self, text: &str) -> NodeId {
        self.push(NodeData::Comment(text.to_string()))
    }

    pub fn create_fragment(&mut self) -> NodeId {
        self.push(NodeData::Fragment)
    }

    /// Add an element to the top of the top layer (re-adding moves it up).
    pub fn top_layer_add(&mut self, id: NodeId, modal: bool) {
        self.top_layer.retain(|&n| n != id);
        self.top_layer.push(id);
        if let Some(e) = self.element_mut(id) {
            e.modal = modal;
        }
        self.touch(StyleRecord::State(id));
    }

    pub fn top_layer_remove(&mut self, id: NodeId) {
        let had = self.top_layer.len();
        self.top_layer.retain(|&n| n != id);
        let was_modal = self.element(id).map(|e| e.modal).unwrap_or(false);
        if let Some(e) = self.element_mut(id) {
            e.modal = false;
        }
        if had != self.top_layer.len() || was_modal {
            self.touch(StyleRecord::State(id));
        }
    }

    /// A CDATA section: a text node serialized as `<![CDATA[...]]>`.
    pub fn create_cdata(&mut self, text: &str) -> NodeId {
        let id = self.create_text(text);
        self.cdata.insert(id);
        id
    }

    /// A processing instruction: a comment-like node with a target.
    pub fn create_pi(&mut self, target: &str, data: &str) -> NodeId {
        let id = self.create_comment(data);
        self.pi_target.insert(id, target.to_string());
        id
    }

    /// DOM nodeType of a node (1 element, 3 text, 4 CDATA, 7 PI, 8 comment, ...).
    pub fn node_type(&self, id: NodeId) -> u8 {
        match &self.nodes[id].data {
            NodeData::Element(_) => 1,
            NodeData::Text(_) => {
                if self.cdata.contains(&id) {
                    4
                } else {
                    3
                }
            }
            NodeData::Comment(_) => {
                if self.pi_target.contains_key(&id) {
                    7
                } else {
                    8
                }
            }
            NodeData::Document => 9,
            NodeData::Doctype(_) => 10,
            NodeData::Fragment => 11,
        }
    }

    pub fn create_doctype(&mut self, name: &str) -> NodeId {
        self.push(NodeData::Doctype(name.to_string()))
    }

    pub fn valid(&self, id: NodeId) -> bool {
        id < self.nodes.len()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match self.nodes.get(id).map(|n| &n.data) {
            Some(NodeData::Element(e)) => Some(e),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match self.nodes.get_mut(id).map(|n| &mut n.data) {
            Some(NodeData::Element(e)) => Some(e),
            _ => None,
        }
    }

    pub fn tag(&self, id: NodeId) -> &str {
        self.element(id).map(|e| e.name.as_str()).unwrap_or("")
    }

    pub fn is_tag(&self, id: NodeId, name: &str) -> bool {
        // An SVG <style> is a style sheet owner like the HTML one.
        self.element(id).map(|e| e.name == name && (e.ns == Namespace::Html || (e.ns == Namespace::Svg && name == "style"))).unwrap_or(false)
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id).and_then(|e| e.attr(name))
    }

    pub fn is_element(&self, id: NodeId) -> bool {
        self.element(id).is_some()
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id)?.parent
    }

    /// True if element `id` has attribute `name`.
    pub fn has_attr(&self, id: NodeId, name: &str) -> bool {
        self.element(id).map(|e| e.has_attr(name)).unwrap_or(false)
    }

    /// Element ancestors of `id`, nearest first.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.parent_element(id), move |&p| self.parent_element(p))
    }

    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        let p = self.parent(id)?;
        if self.is_element(p) {
            Some(p)
        } else {
            None
        }
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes[id].children.iter().copied().filter(move |&c| self.is_element(c))
    }

    fn index_in_parent(&self, id: NodeId) -> Option<(NodeId, usize)> {
        let p = self.parent(id)?;
        let kids = &self.nodes[p].children;
        // Walking siblings asks for neighboring indices: try near the last answer.
        let (hp, hi) = self.sib_hint.get();
        if hp == p {
            for cand in [hi.wrapping_add(1), hi, hi.wrapping_sub(1)] {
                if kids.get(cand) == Some(&id) {
                    self.sib_hint.set((p, cand));
                    return Some((p, cand));
                }
            }
        }
        let idx = kids.iter().position(|&c| c == id)?;
        self.sib_hint.set((p, idx));
        Some((p, idx))
    }

    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        let (p, i) = self.index_in_parent(id)?;
        self.nodes[p].children.get(i + 1).copied()
    }

    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        let (p, i) = self.index_in_parent(id)?;
        if i == 0 {
            None
        } else {
            self.nodes[p].children.get(i - 1).copied()
        }
    }

    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let (p, i) = self.index_in_parent(id)?;
        self.nodes[p].children[i + 1..].iter().copied().find(|&c| self.is_element(c))
    }

    pub fn prev_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let (p, i) = self.index_in_parent(id)?;
        self.nodes[p].children[..i].iter().rev().copied().find(|&c| self.is_element(c))
    }

    /// Is `node` attached to the document tree?
    pub fn is_connected(&self, mut node: NodeId) -> bool {
        let rr = self.render_root.get();
        loop {
            if node == DOCUMENT || node == rr {
                return true;
            }
            match self.parent(node) {
                Some(p) => node = p,
                // A shadow tree is connected when its host is.
                None => match self.host_of(node) {
                    Some(h) => node = h,
                    None => return false,
                },
            }
        }
    }

    /// Is `node` in the document tree itself (connected, and not inside a shadow tree)?
    pub fn in_document_tree(&self, node: NodeId) -> bool {
        self.root_of(node) == DOCUMENT
    }

    pub fn contains(&self, ancestor: NodeId, mut node: NodeId) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.parent(node) {
                Some(p) => node = p,
                None => return false,
            }
        }
    }

    pub fn detach(&mut self, child: NodeId) {
        if let Some(p) = self.nodes[child].parent {
            let kids = &self.nodes[p].children;
            let next = kids.iter().position(|&c| c == child).and_then(|i| kids.get(i + 1).copied());
            self.touch(StyleRecord::Removed(p, next));
            self.nodes[child].parent = None;
            self.nodes[p].children.retain(|&c| c != child);
        }
    }

    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        if child == parent || self.shadow_including_contains(child, parent) {
            return false; // would create a cycle
        }
        if let NodeData::Fragment = self.nodes[child].data {
            let kids = std::mem::take(&mut self.nodes[child].children);
            for k in kids {
                self.nodes[k].parent = None;
                self.append_child(parent, k);
            }
            return true;
        }
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
        self.adopt_on_insert(parent, child);
        self.touch(StyleRecord::Inserted(parent, child));
        true
    }

    /// Insertion adopts the node into its new parent's document.
    fn adopt_on_insert(&mut self, parent: NodeId, child: NodeId) {
        if self.is_document(child) {
            return;
        }
        let o = self.owner_doc(parent);
        if self.nodes[child].owner != o {
            self.set_owner_tree(child, o);
        }
    }

    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) -> bool {
        let reference = match reference {
            Some(r) if r != child => r,
            Some(_) => return true,
            None => return self.append_child(parent, child),
        };
        if child == parent || self.shadow_including_contains(child, parent) {
            return false;
        }
        if let NodeData::Fragment = self.nodes[child].data {
            let kids = std::mem::take(&mut self.nodes[child].children);
            for k in kids {
                self.nodes[k].parent = None;
                self.insert_before(parent, k, Some(reference));
            }
            return true;
        }
        self.detach(child);
        let idx = match self.nodes[parent].children.iter().position(|&c| c == reference) {
            Some(i) => i,
            None => return false,
        };
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.insert(idx, child);
        self.adopt_on_insert(parent, child);
        self.touch(StyleRecord::Inserted(parent, child));
        true
    }

    pub fn remove_all_children(&mut self, parent: NodeId) {
        let kids = std::mem::take(&mut self.nodes[parent].children);
        for k in kids {
            self.nodes[k].parent = None;
        }
        self.touch(StyleRecord::Children(parent));
    }

    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        if let Some(e) = self.element_mut(id) {
            let old = e.attr_exact(name).map(|v| v.to_string());
            e.set_attr(name, value);
            if old.as_deref() == Some(value) {
                self.touch_inert(id);
            } else {
                self.touch(StyleRecord::Attr(id, name.to_string(), old, Some(value.to_string())));
            }
        }
    }

    /// setAttributeNS. Returns the old value.
    pub fn set_attr_ns(&mut self, id: NodeId, ns: Option<&str>, qname: &str, value: &str) -> Option<String> {
        let e = self.element_mut(id)?;
        let old = e.set_attr_ns(ns, qname, value);
        let name = e.attrs.iter().find(|a| a.ns.as_deref() == ns && a.value == value && (a.name == qname || a.local_name() == qname.rsplit(':').next().unwrap_or(qname))).map(|a| a.name.clone()).unwrap_or_else(|| qname.to_string());
        if old.as_deref() == Some(value) {
            self.touch_inert(id);
        } else {
            self.touch(StyleRecord::Attr(id, name, old.clone(), Some(value.to_string())));
        }
        old
    }

    /// removeAttributeNS. Returns the removed (qualified name, value).
    pub fn remove_attr_ns(&mut self, id: NodeId, ns: Option<&str>, local: &str) -> Option<(String, String)> {
        let e = self.element_mut(id)?;
        let r = e.remove_attr_ns(ns, local)?;
        self.touch(StyleRecord::Attr(id, r.0.clone(), Some(r.1.clone()), None));
        Some(r)
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        if let Some(e) = self.element_mut(id) {
            let old = e.attr_exact(name).map(|v| v.to_string());
            if e.remove_attr(name) {
                self.touch(StyleRecord::Attr(id, name.to_string(), old, None));
            }
        }
    }

    pub fn set_text(&mut self, id: NodeId, text: &str) {
        match &mut self.nodes[id].data {
            NodeData::Text(t) | NodeData::Comment(t) => {
                // Only `:empty` of the parent can change (text styles inherit).
                let was_empty = t.is_empty();
                *t = text.to_string();
                match self.nodes[id].parent {
                    Some(p) if was_empty != text.is_empty() => self.touch(StyleRecord::Children(p)),
                    _ => self.touch(StyleRecord::Text(id)),
                }
            }
            _ => {}
        }
    }

    /// Depth-first pre-order traversal of descendants (excluding root).
    pub fn descendants(&self, root: NodeId) -> Descendants<'_> {
        let mut stack: Vec<NodeId> = self.nodes[root].children.iter().rev().copied().collect();
        stack.reserve(32);
        Descendants { doc: self, stack }
    }

    pub fn text_content(&self, id: NodeId) -> String {
        match &self.nodes[id].data {
            NodeData::Text(t) | NodeData::Comment(t) => t.clone(),
            _ => {
                let mut s = String::new();
                for d in self.descendants(id) {
                    if let NodeData::Text(t) = &self.nodes[d].data {
                        s.push_str(t);
                    }
                }
                s
            }
        }
    }

    /// "Child text content": the data of the node's Text children only
    /// (what <style> and <script> read).
    pub fn child_text_content(&self, id: NodeId) -> String {
        let mut s = String::new();
        for &c in self.children(id) {
            if let NodeData::Text(t) = &self.nodes[c].data {
                s.push_str(t);
            }
        }
        s
    }

    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        match self.nodes[id].data {
            NodeData::Text(_) | NodeData::Comment(_) => self.set_text(id, text),
            _ => {
                self.remove_all_children(id);
                if !text.is_empty() {
                    let t = self.create_text(text);
                    self.append_child(id, t);
                }
            }
        }
    }

    pub fn document_element(&self) -> Option<NodeId> {
        self.element_children(self.render_root.get()).next()
    }

    pub fn find_child_tag(&self, parent: NodeId, name: &str) -> Option<NodeId> {
        self.element_children(parent).find(|&c| self.is_tag(c, name))
    }

    pub fn head(&self) -> Option<NodeId> {
        self.document_element().and_then(|h| self.find_child_tag(h, "head"))
    }

    pub fn body(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.find_child_tag(html, "body").or_else(|| self.find_child_tag(html, "frameset"))
    }

    pub fn title(&self) -> String {
        if let Some(t) = &self.title_override {
            return t.clone();
        }
        for d in self.descendants(DOCUMENT) {
            if self.is_tag(d, "title") {
                return collapse_ws(&self.text_content(d));
            }
            if self.is_tag(d, "body") {
                break;
            }
        }
        // SVG/other documents: look anywhere.
        String::new()
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.id_index().by_id.get(id).copied()
    }

    pub fn elements_by_tag(&self, root: NodeId, tag: &str) -> Vec<NodeId> {
        self.elements_by_qualified_name(root, tag, !self.is_xml)
    }

    /// DOM "list of elements with qualified name": HTML elements of an HTML
    /// document match the lowercased name, everything else matches exactly.
    pub fn elements_by_qualified_name(&self, root: NodeId, tag: &str, html_doc: bool) -> Vec<NodeId> {
        if tag == "*" {
            return self.descendants(root).filter(|&n| self.element(n).is_some()).collect();
        }
        let lower = tag.to_ascii_lowercase();
        self.descendants(root)
            .filter(|&n| {
                self.element(n)
                    .map(|e| match e.ns {
                        Namespace::Html if html_doc => e.name == lower,
                        Namespace::Svg => svg_tag_case(&e.name) == tag,
                        _ => e.name == tag,
                    })
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Resolve a (possibly relative) URL against the document base.
    /// The document base URL: the first <base href> (in the head), resolved
    /// against the fallback base URL; else the fallback (cached per DOM generation).
    pub fn current_base(&self) -> Url {
        if self.render_root.get() != DOCUMENT {
            if let Some(u) = self.render_base.borrow().as_ref() {
                return u.clone();
            }
        }
        if let Some((g, u)) = self.base_cache.borrow().as_ref() {
            if *g == self.generation {
                return u.clone();
            }
        }
        let fallback = self.fallback_base.clone().unwrap_or_else(|| self.url.clone());
        let mut base = None;
        if let Some(head) = self.head() {
            for e in self.descendants(head) {
                if self.is_tag(e, "base") {
                    if let Some(h) = self.attr(e, "href") {
                        base = fallback.join(h.trim_matches(|c: char| c <= ' ')).ok();
                        break;
                    }
                }
            }
        }
        // Without a head <base>, the base set at parse time (or by the page) stands.
        let u = base.unwrap_or_else(|| if self.base_url != self.url { self.base_url.clone() } else { fallback });
        *self.base_cache.borrow_mut() = Some((self.generation, u.clone()));
        u
    }

    pub fn resolve_url(&self, href: &str) -> Option<Url> {
        // The URL parser strips C0 controls and spaces only (not U+3000 etc.).
        let href = href.trim_matches(|c: char| c <= ' ');
        let base = self.current_base();
        if self.charset == "UTF-8" || !href.contains('?') {
            return base.join(href).ok();
        }
        // A legacy-encoded document encodes URL queries in its encoding
        // (unmappable characters as percent-encoded numeric references).
        let cs = self.charset;
        let enc = crate::html::encoding::override_fn(move |q: &str| {
            std::borrow::Cow::Owned(crate::html::encoding::encode_with(cs, q, &|c, out: &mut Vec<u8>| out.extend_from_slice(format!("%26%23{}%3B", c as u32).as_bytes())))
        });
        Url::options().base_url(Some(&base)).encoding_override(Some(&enc)).parse(href).ok()
    }

    /// The contents fragment of a `<template>` element, moving any children it
    /// still has (from parsing or DOM insertion) into it.
    pub fn template_content(&mut self, t: NodeId) -> NodeId {
        let frag = match self.template_contents.get(&t) {
            Some(&f) => f,
            None => {
                let f = self.create_fragment();
                self.template_contents.insert(t, f);
                f
            }
        };
        let kids: Vec<NodeId> = self.nodes[t].children.clone();
        for k in kids {
            self.append_child(frag, k);
        }
        frag
    }

    /// Move the children of every `<template>` under `root` (including inside
    /// template contents) into their contents fragments, making them inert.
    pub fn hoist_template_contents(&mut self, root: NodeId) {
        let mut todo = vec![root];
        while let Some(r) = todo.pop() {
            let temps: Vec<NodeId> = self.descendants(r).filter(|&d| self.element(d).map(|e| e.name == "template" && e.ns == Namespace::Html).unwrap_or(false)).collect();
            for t in temps {
                if !self.nodes[t].children.is_empty() || !self.template_contents.contains_key(&t) {
                    let f = self.template_content(t);
                    todo.push(f);
                }
            }
        }
    }

    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        // A document clones into a new document that owns its cloned children.
        if self.is_document(id) {
            let d = self.create_document_node();
            if deep {
                let kids = self.nodes[id].children.clone();
                for k in kids {
                    let c = self.clone_node(k, true);
                    self.set_owner_tree(c, d);
                    self.nodes[c].parent = Some(d);
                    self.nodes[d].children.push(c);
                }
            }
            return d;
        }
        let data = self.nodes[id].data.clone();
        let new = self.push(data);
        self.nodes[new].owner = self.owner_doc(id);
        if let Some(u) = self.ns_uris.get(&id).cloned() {
            self.ns_uris.insert(new, u);
        }
        if self.cdata.contains(&id) {
            self.cdata.insert(new);
        }
        if let Some(t) = self.pi_target.get(&id).cloned() {
            self.pi_target.insert(new, t);
        }
        if deep {
            let kids = self.nodes[id].children.clone();
            for k in kids {
                let c = self.clone_node(k, true);
                self.nodes[c].parent = Some(new);
                self.nodes[new].children.push(c);
            }
        }
        // A template's contents are cloned along with it (when deep).
        if let Some(&f) = self.template_contents.get(&id) {
            if deep {
                let cf = self.clone_node(f, true);
                self.template_contents.insert(new, cf);
            }
        }
        // A clonable shadow root comes along with its host.
        if let Some(&r) = self.shadow_roots.get(&id) {
            if let Some(info) = self.shadow_info.get(&r).cloned().filter(|i| i.clonable) {
                if let Some(nr) = self.attach_shadow(new, info) {
                    let kids = self.nodes[r].children.clone();
                    for k in kids {
                        let c = self.clone_node(k, true);
                        self.nodes[c].parent = Some(nr);
                        self.nodes[nr].children.push(c);
                    }
                }
            }
        }
        new
    }

    // ----------------------------------------------------------- serialization

    pub fn inner_html(&self, id: NodeId) -> String {
        let mut s = String::new();
        let id = self.template_contents.get(&id).copied().unwrap_or(id);
        for &c in &self.nodes[id].children {
            self.serialize_into(c, &mut s, self.raw_text_parent(id));
        }
        s
    }

    pub fn outer_html(&self, id: NodeId) -> String {
        let mut s = String::new();
        let raw = self.parent(id).map(|p| self.raw_text_parent(p)).unwrap_or(false);
        self.serialize_into(id, &mut s, raw);
        s
    }

    fn raw_text_parent(&self, id: NodeId) -> bool {
        matches!(
            self.tag(id),
            "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext" | "noscript"
        )
    }

    /// innerHTML with shadow roots (getHTML()): `include(root)` picks the
    /// shadow roots serialized as declarative `<template shadowrootmode>`.
    pub fn inner_html_with(&self, id: NodeId, include: &dyn Fn(NodeId, &ShadowInfo) -> bool) -> String {
        let mut s = String::new();
        let id = self.template_contents.get(&id).copied().unwrap_or(id);
        if !self.shadow_info.contains_key(&id) {
            self.shadow_template(id, &mut s, include);
        }
        for &c in &self.nodes[id].children {
            self.serialize_sh(c, &mut s, self.raw_text_parent(id), Some(include));
        }
        s
    }

    fn shadow_template(&self, host: NodeId, s: &mut String, include: &dyn Fn(NodeId, &ShadowInfo) -> bool) {
        let Some(&root) = self.shadow_roots.get(&host) else { return };
        let Some(info) = self.shadow_info.get(&root) else { return };
        if !include(root, info) {
            return;
        }
        s.push_str("<template shadowrootmode=\"");
        s.push_str(if info.open { "open" } else { "closed" });
        s.push('"');
        if info.delegates_focus {
            s.push_str(" shadowrootdelegatesfocus=\"\"");
        }
        if info.serializable {
            s.push_str(" shadowrootserializable=\"\"");
        }
        if info.manual {
            s.push_str(" shadowrootslotassignment=\"manual\"");
        }
        if info.clonable {
            s.push_str(" shadowrootclonable=\"\"");
        }
        s.push('>');
        for &c in &self.nodes[root].children {
            self.serialize_sh(c, s, false, Some(include));
        }
        s.push_str("</template>");
    }

    fn serialize_into(&self, id: NodeId, s: &mut String, raw_parent: bool) {
        self.serialize_sh(id, s, raw_parent, None)
    }

    fn serialize_sh(&self, id: NodeId, s: &mut String, raw_parent: bool, sh: Option<&dyn Fn(NodeId, &ShadowInfo) -> bool>) {
        match &self.nodes[id].data {
            NodeData::Text(t) => {
                if self.cdata.contains(&id) {
                    s.push_str("<![CDATA[");
                    s.push_str(t);
                    s.push_str("]]>");
                } else if raw_parent {
                    s.push_str(t)
                } else {
                    escape_text(t, s)
                }
            }
            NodeData::Comment(t) => {
                if let Some(target) = self.pi_target.get(&id) {
                    s.push_str("<?");
                    s.push_str(target);
                    s.push(' ');
                    s.push_str(t);
                    s.push('>');
                } else {
                    s.push_str("<!--");
                    s.push_str(t);
                    s.push_str("-->");
                }
            }
            NodeData::Doctype(n) => {
                s.push_str("<!DOCTYPE ");
                s.push_str(n.split('\0').next().unwrap_or(""));
                s.push('>');
            }
            NodeData::Document | NodeData::Fragment => {
                for &c in &self.nodes[id].children {
                    self.serialize_sh(c, s, false, sh);
                }
            }
            NodeData::Element(e) => {
                let name = if e.ns == Namespace::Svg { svg_tag_case(&e.name) } else { e.name.as_str() };
                s.push('<');
                s.push_str(name);
                for a in &e.attrs {
                    s.push(' ');
                    let an = if e.ns == Namespace::Svg { svg_attr_case(&a.name) } else { a.name.as_str() };
                    s.push_str(an);
                    s.push_str("=\"");
                    escape_attr(&a.value, s);
                    s.push('"');
                }
                s.push('>');
                if e.ns == Namespace::Html && is_void(&e.name) {
                    return;
                }
                let raw = e.ns == Namespace::Html && self.raw_text_parent(id);
                let src = self.template_contents.get(&id).copied().unwrap_or(id);
                if let Some(f) = sh {
                    self.shadow_template(id, s, f);
                }
                for &c in &self.nodes[src].children {
                    self.serialize_sh(c, s, raw, sh);
                }
                s.push_str("</");
                s.push_str(name);
                s.push('>');
            }
        }
    }

    /// Serialize an inline <svg> subtree as standalone XML for the SVG renderer.
    /// Serialize an inline `<svg>` subtree; `over(node)` gives attributes that
    /// replace (or add to) that element's own, e.g. CSS `fill` beating a
    /// presentation attribute.
    pub fn svg_markup_with(&self, id: NodeId, over: &dyn Fn(NodeId) -> Vec<(&'static str, String)>) -> String {
        let mut s = String::new();
        self.svg_into(id, &mut s, true, over);
        s
    }

    /// Serialize an SVG element (not as a document root) for copying into
    /// another SVG's `<defs>`.
    pub fn svg_fragment_with(&self, id: NodeId, over: &dyn Fn(NodeId) -> Vec<(&'static str, String)>) -> String {
        let mut s = String::new();
        self.svg_into(id, &mut s, false, over);
        s
    }

    fn svg_into(&self, id: NodeId, s: &mut String, root: bool, over: &dyn Fn(NodeId) -> Vec<(&'static str, String)>) {
        match &self.nodes[id].data {
            NodeData::Text(t) => escape_text(t, s),
            NodeData::Element(e) => {
                let name = svg_tag_case(&e.name);
                let extra = over(id);
                // A stand-in for content the SVG renderer can't draw
                // (foreignObject HTML, given as SVG text by the caller).
                if let Some((_, rep)) = extra.iter().find(|(n, _)| *n == "__replace") {
                    s.push_str(rep);
                    return;
                }
                // Inline <style>: declarations with var() were resolved by the
                // page's cascade (and exported as attributes); the renderer
                // can't resolve them, so they are dropped here.
                if name == "style" && !root {
                    let css: String = self.nodes[id].children.iter().filter_map(|&c| match &self.nodes[c].data {
                        NodeData::Text(t) => Some(t.as_str()),
                        _ => None,
                    }).collect();
                    if css.contains("var(") {
                        s.push_str("<style>");
                        escape_text(&strip_var_decls(&css), s);
                        s.push_str("</style>");
                        return;
                    }
                }
                s.push('<');
                s.push_str(name);
                let mut has_xmlns = false;
                for a in &e.attrs {
                    if a.name == "xmlns" {
                        has_xmlns = true;
                    }
                    if extra.iter().any(|(n, _)| *n == a.name) {
                        continue;
                    }
                    s.push(' ');
                    s.push_str(svg_attr_case(&a.name));
                    s.push_str("=\"");
                    escape_attr(&a.value, s);
                    s.push('"');
                }
                for (n, v) in &extra {
                    s.push(' ');
                    s.push_str(n);
                    s.push_str("=\"");
                    escape_attr(v, s);
                    s.push('"');
                }
                if root && !has_xmlns {
                    s.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
                }
                if root && !e.has_attr("xmlns:xlink") {
                    s.push_str(" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
                }
                s.push('>');
                for &c in &self.nodes[id].children {
                    self.svg_into(c, s, false, over);
                }
                s.push_str("</");
                s.push_str(name);
                s.push('>');
            }
            _ => {}
        }
    }
}

pub struct Descendants<'a> {
    doc: &'a Document,
    stack: Vec<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let n = self.stack.pop()?;
        for &c in self.doc.nodes[n].children.iter().rev() {
            self.stack.push(c);
        }
        Some(n)
    }
}

/// Drop CSS declarations that use var() (keeping rules and at-rules).
pub fn strip_var_decls(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut decl = String::new();
    let mut depth = 0usize;
    // Whether the current block holds declarations (vs. nested rules).
    let mut in_decls = false;
    let mut prelude = String::new();
    for c in css.chars() {
        match c {
            '{' => {
                // Text read so far at this level was a selector / at-rule prelude.
                out.push_str(&prelude);
                out.push_str(&decl);
                prelude.clear();
                decl.clear();
                out.push('{');
                depth += 1;
                in_decls = true;
            }
            '}' => {
                if in_decls && !decl.contains("var(") {
                    out.push_str(&decl);
                }
                decl.clear();
                out.push('}');
                depth = depth.saturating_sub(1);
                in_decls = false;
            }
            ';' if depth > 0 => {
                if !decl.contains("var(") {
                    out.push_str(&decl);
                    out.push(';');
                }
                decl.clear();
            }
            _ => {
                if depth == 0 {
                    prelude.push(c);
                } else {
                    decl.push(c);
                }
            }
        }
    }
    out.push_str(&prelude);
    out.push_str(&decl);
    out
}

pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape_text(t: &str, s: &mut String) {
    for c in t.chars() {
        match c {
            '&' => s.push_str("&amp;"),
            '<' => s.push_str("&lt;"),
            '>' => s.push_str("&gt;"),
            '\u{a0}' => s.push_str("&nbsp;"),
            _ => s.push(c),
        }
    }
}

fn escape_attr(t: &str, s: &mut String) {
    for c in t.chars() {
        match c {
            '&' => s.push_str("&amp;"),
            '"' => s.push_str("&quot;"),
            '<' => s.push_str("&lt;"),
            '>' => s.push_str("&gt;"),
            '\u{a0}' => s.push_str("&nbsp;"),
            _ => s.push(c),
        }
    }
}

/// HTML lower-cases tag names; SVG needs its camelCase names back.
pub fn svg_tag_case(name: &str) -> &str {
    const TAGS: &[&str] = &[
        "altGlyph", "altGlyphDef", "altGlyphItem", "animateColor", "animateMotion", "animateTransform", "clipPath",
        "feBlend", "feColorMatrix", "feComponentTransfer", "feComposite", "feConvolveMatrix", "feDiffuseLighting",
        "feDisplacementMap", "feDistantLight", "feDropShadow", "feFlood", "feFuncA", "feFuncB", "feFuncG", "feFuncR",
        "feGaussianBlur", "feImage", "feMerge", "feMergeNode", "feMorphology", "feOffset", "fePointLight",
        "feSpecularLighting", "feSpotLight", "feTile", "feTurbulence", "foreignObject", "glyphRef", "linearGradient",
        "radialGradient", "textPath",
    ];
    TAGS.iter().find(|t| t.eq_ignore_ascii_case(name)).copied().unwrap_or(name)
}

pub fn svg_attr_case(name: &str) -> &str {
    const ATTRS: &[&str] = &[
        "attributeName", "attributeType", "baseFrequency", "baseProfile", "calcMode", "clipPathUnits",
        "diffuseConstant", "edgeMode", "filterUnits", "glyphRef", "gradientTransform", "gradientUnits",
        "kernelMatrix", "kernelUnitLength", "keyPoints", "keySplines", "keyTimes", "lengthAdjust",
        "limitingConeAngle", "markerHeight", "markerUnits", "markerWidth", "maskContentUnits", "maskUnits",
        "numOctaves", "pathLength", "patternContentUnits", "patternTransform", "patternUnits", "pointsAtX",
        "pointsAtY", "pointsAtZ", "preserveAlpha", "preserveAspectRatio", "primitiveUnits", "refX", "refY",
        "repeatCount", "repeatDur", "requiredExtensions", "requiredFeatures", "specularConstant",
        "specularExponent", "spreadMethod", "startOffset", "stdDeviation", "stitchTiles", "surfaceScale",
        "systemLanguage", "tableValues", "targetX", "targetY", "textLength", "viewBox", "viewTarget",
        "xChannelSelector", "yChannelSelector", "zoomAndPan",
    ];
    ATTRS.iter().find(|t| t.eq_ignore_ascii_case(name)).copied().unwrap_or(name)
}
