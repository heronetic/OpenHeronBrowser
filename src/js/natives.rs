// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Native functions behind `__n`: the DOM, layout queries, navigation,
//! storage, networking and canvas, exposed as plain data (see `host.rs`).
//! Nothing here depends on the JavaScript engine.

use super::host::*;
use crate::dom::{NodeData, NodeId, DOCUMENT};
use crate::page::{with_page, JsTask, NetMsg, PageMsg};

macro_rules! native {
    ($name:ident, |$args:ident| $body:block) => {
        #[allow(unused_variables, unused_mut)]
        fn $name($args: &mut dyn HostArgs) -> HostResult {
            $body
        }
    };
}

// ------------------------------------------------------------------ helpers



// ------------------------------------------------------------ tree natives

native!(n_type, |a| {
    let id = nid(a, 0);
    Ok(HostValue::from(
        id.and_then(|id| with_page(|p| p.doc.node_type(id) as i32))
        .unwrap_or(0),
    ))
});

native!(n_name, |a| {
    let id = nid(a, 0);
    Ok(jstr(&id.and_then(|id| with_page(|p| match &p.doc.node(id).data {
        NodeData::Element(e) => e.name.clone(),
        NodeData::Doctype(n) => n.split('\0').next().unwrap_or("").to_string(),
        _ => String::new(),
    })).unwrap_or_default()))
});

native!(n_ns, |a| {
    let id = nid(a, 0);
    let r = id.and_then(|id| with_page(|p| p.doc.element(id).map(|e| match e.ns {
        crate::dom::Namespace::Svg => Some("http://www.w3.org/2000/svg".to_string()),
        crate::dom::Namespace::MathMl => Some("http://www.w3.org/1998/Math/MathML".to_string()),
        crate::dom::Namespace::Html => Some("http://www.w3.org/1999/xhtml".to_string()),
        crate::dom::Namespace::Other => p.doc.ns_uris.get(&id).filter(|u| !u.is_empty()).cloned(),
    }))).flatten().flatten();
    Ok(jopt(r))
});

native!(n_doctype_ids, |a| {
    let id = nid(a, 0);
    let v = id.and_then(|id| with_page(|p| match &p.doc.node(id).data {
        NodeData::Doctype(n) => {
            let mut it = n.split('\0');
            it.next();
            Some(vec![it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string()])
        }
        _ => None,
    })).flatten().unwrap_or_else(|| vec![String::new(), String::new()]);
    Ok(jstrs(v))
});

// Parse a complete HTML document into a sub-document node (document.open/write/close).
native!(n_parse_into, |a| {
    // (document, html, scripting) -> whether the result is in quirks mode.
    let target = nid(a, 0);
    let html = st(a, 1);
    let scripting = boolean(a, 2, false);
    let mut quirks = false;
    if let Some(t) = target {
        with_page(|p| {
            if t == DOCUMENT || !p.doc.is_document(t) {
                return;
            }
            let tmp = crate::html::parse_document_with(&html, p.doc.url.clone(), scripting);
            quirks = tmp.quirks;
            p.doc.sub_quirks.insert(t, quirks);
            p.doc.remove_all_children(t);
            let kids = tmp.children(DOCUMENT).to_vec();
            for k in kids {
                let n = p.doc.import_from(&tmp, k, t);
                p.doc.append_child(t, n);
            }
        });
    }
    Ok(HostValue::Bool(quirks))
});

native!(n_doc_is_xml, |a| { Ok(HostValue::from(with_page(|p| p.doc.is_xml).unwrap_or(false))) });
native!(n_doc_content_type, |a| { Ok(jstr(&with_page(|p| p.doc_content_type.clone()).unwrap_or_default())) });

native!(n_parse_xml_into, |a| {
    // Parse XML into an (empty) document node; returns the error text or null.
    let target = nid(a, 0);
    let src = st(a, 1);
    let mut err = None;
    if let Some(t) = target {
        with_page(|p| {
            if t == DOCUMENT || !p.doc.is_document(t) {
                return;
            }
            p.doc.remove_all_children(t);
            if let Err(e) = crate::html::xml::parse_into_node(&mut p.doc, t, &src) {
                crate::html::xml::insert_parser_error_at(&mut p.doc, t, &e);
                err = Some(format!("error on line {} at column {}: {}", e.line, e.col, e.msg));
            }
        });
    }
    Ok(err.map(|e| jstr(&e)).unwrap_or(HostValue::Null))
});

native!(n_create_doctype, |a| {
    let name = st(a, 0);
    let owner = owner_arg(a, 1);
    Ok(jid(with_page(|p| {
        let id = p.doc.create_doctype(&name);
        if let Some(o) = owner {
            p.doc.nodes[id].owner = o;
        }
        id
    })))
});

native!(n_parent, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.parent(id)).flatten())))
});

native!(n_first_child, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.children(id).first().copied())).flatten()))
});

native!(n_last_child, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.children(id).last().copied())).flatten()))
});

native!(n_children, |a| {
    let id = nid(a, 0);
    let v = id.and_then(|id| with_page(|p| p.doc.children(id).to_vec())).unwrap_or_default();
    Ok(jids(v))
});

native!(n_elchildren, |a| {
    let id = nid(a, 0);
    let v = id.and_then(|id| with_page(|p| p.doc.element_children(id).collect::<Vec<_>>())).unwrap_or_default();
    Ok(jids(v))
});

native!(n_next, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.next_sibling(id)).flatten())))
});

native!(n_prev, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.prev_sibling(id)).flatten())))
});

native!(n_next_el, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.next_element_sibling(id)).flatten())))
});

native!(n_prev_el, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.prev_element_sibling(id)).flatten())))
});

native!(n_text, |a| {
    let id = nid(a, 0);
    Ok(jstr(&id.and_then(|id| with_page(|p| p.doc.text_content(id))).unwrap_or_default()))
});

native!(n_set_text, |a| {
    let id = nid(a, 0);
    let s = st(a, 1);
    if let Some(id) = id {
        with_page(|p| {
            p.doc.set_text_content(id, &s);
            style_node_changed(p, id);
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_inner_text, |a| {
    let id = nid(a, 0);
    Ok(jstr(&id.and_then(|id| with_page(|p| inner_text(p, id))).unwrap_or_default()))
});

native!(n_attr, |a| {
    let id = nid(a, 0);
    let name = st(a, 1);
    Ok(jopt(id.and_then(|id| {
        // Exact qualified-name match (script lowercases for HTML elements).
        with_page(|p| p.doc.element(id).and_then(|e| e.attr_exact(&name)).map(|s| s.to_string()))
        .flatten()
    })))
});

native!(n_set_attr, |a| {
    let id = nid(a, 0);
    let name = st(a, 1);
    let val = st(a, 2);
    if let Some(id) = id {
        with_page(|p| {
            if name.is_empty() {
                return;
            }
            let prev = p.doc.attr(id, &name).map(|s| s.to_string());
            let unchanged = prev.as_deref() == Some(val.as_str());
            p.doc.set_attr(id, &name, &val);
            p.img_attr_mutation(id, &name, prev.as_deref(), Some(val.as_str()));
            if name == "src" && p.doc.is_tag(id, "script") && p.doc.is_connected(id) {
                p.add_script_node(id);
                p.js_tasks.push(JsTask::RunScripts);
            }
            if (name == "href" || name == "rel") && p.doc.is_tag(id, "link") && p.doc.is_connected(id) {
                if p.doc.attr(id, "rel").map(|r| r.to_ascii_lowercase().contains("stylesheet")).unwrap_or(false) {
                    p.add_sheet_node(id, true);
                }
            }
            // media/disabled/rel are evaluated per cascade: just restyle.
            if !unchanged && matches!(name.as_str(), "media" | "disabled" | "rel") && (p.doc.is_tag(id, "link") || p.doc.is_tag(id, "style")) {
                p.restyle_sheets();
            }
            if matches!(name.as_str(), "src" | "srcset") && p.doc.is_tag(id, "img") {
                p.img_src_changed(id);
            }
            if name == "src" && p.doc.is_tag(id, "iframe") && p.doc.is_connected(id) && p.frame_views_has(id) {
                p.frame_reload.insert(id);
                p.need_layout = true;
            }
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_remove_attr, |a| {
    let id = nid(a, 0);
    let name = st(a, 1);
    if let Some(id) = id {
        with_page(|p| {
            let prev = p.doc.attr(id, &name).map(|s| s.to_string());
            p.doc.remove_attr(id, &name);
            if prev.is_some() {
                p.img_attr_mutation(id, &name, prev.as_deref(), None);
            }
            if matches!(name.as_str(), "media" | "disabled") && (p.doc.is_tag(id, "link") || p.doc.is_tag(id, "style")) {
                p.restyle_sheets();
            }
        });
    }
    Ok(HostValue::Undefined)
});

// Namespaces of an element's attributes, parallel to `attrs` (null = no namespace).
native!(n_attr_nss, |a| {
    let id = nid(a, 0);
    let v: Vec<HostValue> = id
        .and_then(|id| with_page(|p| p.doc.element(id).map(|e| e.attrs.iter().map(|at| jopt(at.ns.as_deref().map(|s| s.to_string()))).collect::<Vec<_>>())))
        .flatten()
        .unwrap_or_default();
    Ok(HostValue::Array(v))
});

// Index of the (namespace, local name) attribute, or -1.
native!(n_attr_index_ns, |a| {
    let id = nid(a, 0);
    let ns = opt_st(a, 1).filter(|s| !s.is_empty());
    let local = st(a, 2);
    let i = id.and_then(|id| with_page(|p| p.doc.element(id).and_then(|e| e.attr_index_ns(ns.as_deref(), &local))).flatten());
    Ok(HostValue::Number(i.map(|i| i as f64).unwrap_or(-1.0)))
});

// setAttributeNS(ns, qualifiedName, value) (already validated); returns the old value.
native!(n_set_attr_ns, |a| {
    let id = nid(a, 0);
    let ns = opt_st(a, 1).filter(|s| !s.is_empty());
    let qname = st(a, 2);
    let val = st(a, 3);
    let old = id.and_then(|id| with_page(|p| p.doc.set_attr_ns(id, ns.as_deref(), &qname, &val)).flatten());
    Ok(jopt(old))
});

// removeAttributeNS(ns, localName); returns [qualifiedName, oldValue] or null.
native!(n_remove_attr_ns, |a| {
    let id = nid(a, 0);
    let ns = opt_st(a, 1).filter(|s| !s.is_empty());
    let local = st(a, 2);
    let r = id.and_then(|id| with_page(|p| p.doc.remove_attr_ns(id, ns.as_deref(), &local)).flatten());
    Ok(match r {
        Some((n, v)) => jstrs(vec![n, v]),
        None => HostValue::Null,
    })
});

// Top layer: op 1 = add as modal, 2 = add (popover), 0 = remove. Returns the layer.
native!(n_top_layer, |a| {
    let id = nid(a, 0);
    let op = num(a, 1) as i32;
    let v = with_page(|p| {
        if let Some(id) = id {
            match op {
                1 => p.doc.top_layer_add(id, true),
                2 => p.doc.top_layer_add(id, false),
                _ => p.doc.top_layer_remove(id),
            }
            p.need_style = true;
        }
        p.doc.top_layer.clone()
    })
    .unwrap_or_default();
    Ok(jids(v))
});

// Popover showing state (`:popover-open`).
native!(n_set_popover_open, |a| {
    let id = nid(a, 0);
    let open = boolean(a, 1, false);
    if let Some(id) = id {
        with_page(|p| {
            let changed = p.doc.element_mut(id).map(|e| std::mem::replace(&mut e.popover_open, open) != open).unwrap_or(false);
            if changed {
                p.doc.touch(crate::dom::StyleRecord::State(id));
                p.need_style = true;
            }
        });
    }
    Ok(HostValue::Undefined)
});

// The DOM mutation generation (XPath iterator invalidation).
native!(n_dom_gen, |_a| {
    Ok(HostValue::Number(with_page(|p| p.doc.generation as f64).unwrap_or(0.0)))
});

native!(n_attrs, |a| {
    let id = nid(a, 0);
    let v: Vec<String> = id
        .and_then(|id| with_page(|p| p.doc.element(id).map(|e| e.attrs.iter().flat_map(|at| [at.name.clone(), at.value.clone()]).collect::<Vec<_>>())))
        .flatten()
        .unwrap_or_default();
    Ok(jstrs(v))
});

native!(n_create, |a| {
    let tag = st(a, 0);
    let ns = st(a, 1);
    let owner = owner_arg(a, 2);
    let id = with_page(|p| {
        let nsv = if ns == "http://www.w3.org/2000/svg" || ns == "svg" {
            crate::dom::Namespace::Svg
        } else if ns == "http://www.w3.org/1998/Math/MathML" {
            crate::dom::Namespace::MathMl
        } else if ns == "html" || ns == "http://www.w3.org/1999/xhtml" {
            crate::dom::Namespace::Html
        } else {
            crate::dom::Namespace::Other
        };
        let name = if nsv == crate::dom::Namespace::Html && ns == "html" { tag.to_ascii_lowercase() } else { tag.clone() };
        let id = p.doc.create_element(&name, nsv, Vec::new());
        if nsv == crate::dom::Namespace::Html && name == "script" {
            p.script_force_async.insert(id);
        }
        if nsv == crate::dom::Namespace::Other {
            p.doc.ns_uris.insert(id, ns.clone());
        }
        if let Some(o) = owner {
            p.doc.nodes[id].owner = o;
        }
        id
    });
    Ok(jid(id))
});

/// Optional owner-document argument (a Document node id) for node creation.
fn owner_arg(args: &mut dyn HostArgs, i: usize) -> Option<NodeId> {
    nid(args, i).filter(|&o| o != DOCUMENT && with_page(|p| p.doc.is_document(o)).unwrap_or(false))
}

native!(n_create_document, |a| {
    let frame = nid(a, 0);
    Ok(jid(with_page(|p| {
        let d = p.doc.create_document_node();
        if let Some(f) = frame.filter(|&f| p.doc.is_element(f)) {
            p.doc.frame_of.insert(d, f);
        }
        d
    })))
});

native!(n_owner, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| if p.doc.is_document(id) { None } else { Some(p.doc.owner_doc(id)) })).flatten()))
});

native!(n_root, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.root_of(id)))))
});

native!(n_create_text, |a| {
    let s = st(a, 0);
    let owner = owner_arg(a, 1);
    Ok(jid(with_page(|p| {
        let id = p.doc.create_text(&s);
        if let Some(o) = owner {
            p.doc.nodes[id].owner = o;
        }
        id
    })))
});

native!(n_create_comment, |a| {
    let s = st(a, 0);
    let owner = owner_arg(a, 1);
    Ok(jid(with_page(|p| {
        let id = p.doc.create_comment(&s);
        if let Some(o) = owner {
            p.doc.nodes[id].owner = o;
        }
        id
    })))
});

native!(n_create_cdata, |a| {
    let s = st(a, 0);
    let owner = owner_arg(a, 1);
    Ok(jid(with_page(|p| {
        let id = p.doc.create_cdata(&s);
        if let Some(o) = owner {
            p.doc.nodes[id].owner = o;
        }
        id
    })))
});

native!(n_create_pi, |a| {
    let target = st(a, 0);
    let data = st(a, 1);
    let owner = owner_arg(a, 2);
    Ok(jid(with_page(|p| {
        let id = p.doc.create_pi(&target, &data);
        if let Some(o) = owner {
            p.doc.nodes[id].owner = o;
        }
        id
    })))
});

native!(n_pi_target, |a| {
    let id = nid(a, 0);
    Ok(jstr(&id.and_then(|id| with_page(|p| p.doc.pi_target.get(&id).cloned()).flatten()).unwrap_or_default()))
});

native!(n_create_fragment, |a| {
    let owner = owner_arg(a, 0);
    Ok(jid(with_page(|p| {
        let id = p.doc.create_fragment();
        if let Some(o) = owner {
            p.doc.nodes[id].owner = o;
        }
        id
    })))
});

fn after_insert(p: &mut crate::page::Page, child_nodes: &[NodeId]) {
    // Scripts and stylesheets inserted into the document become active.
    for &n in child_nodes {
        if !p.doc.is_connected(n) {
            continue;
        }
        let mut list: Vec<NodeId> = vec![n];
        list.extend(p.doc.descendants(n));
        for d in list {
            match p.doc.tag(d) {
                "script" if p.doc.element(d).map(|e| e.ns == crate::dom::Namespace::Html).unwrap_or(false) => {
                    if !p.scripts.iter().any(|s| s.node == d) {
                        p.add_script_node(d);
                        let ordered = !p.script_force_async.contains(&d) && p.doc.attr(d, "async").is_none() && p.doc.attr(d, "src").is_some();
                        if let Some(s) = p.scripts.last_mut() {
                            if s.node == d {
                                s.asynchronous = true;
                                s.in_order = ordered && !s.module;
                            }
                        }
                        p.js_tasks.push(JsTask::RunScripts);
                    }
                }
                "style" => p.add_sheet_node(d, false),
                "link" => {
                    let rel = p.doc.attr(d, "rel").map(|r| r.to_ascii_lowercase()).unwrap_or_default();
                    if rel.split_whitespace().any(|x| x == "stylesheet") {
                        p.add_sheet_node(d, true);
                    }
                    if rel.split_whitespace().any(|x| x == "modulepreload") {
                        if let Some(u) = p.doc.attr(d, "href").and_then(|h| p.doc.resolve_url(h.trim())) {
                            crate::js::modules::prefetch_module(u, Some(p.doc.url.clone()), 1);
                        }
                    }
                    if rel.split_whitespace().any(|x| x == "preload") {
                        p.preload_link(d);
                    }
                }
                // (An inserted about:blank iframe fires load synchronously: see
                // frameInsertedLoads in the prelude.)
                _ => {}
            }
        }
    }
}

fn style_node_changed(p: &mut crate::page::Page, id: NodeId) {
    let target = if p.doc.is_tag(id, "style") { Some(id) } else { p.doc.parent_element(id).filter(|&x| p.doc.is_tag(x, "style")) };
    if let Some(s) = target {
        // New contents replace any CSSOM-edited rules.
        p.style_override.remove(&s);
        if p.doc.is_connected(s) {
            p.add_sheet_node(s, false);
        }
    }
}

native!(n_append, |a| {
    let parent = nid(a, 0);
    let child = nid(a, 1);
    let ok = match (parent, child) {
        (Some(pa), Some(ch)) => with_page(|p| {
            let kids: Vec<NodeId> = if matches!(p.doc.node(ch).data, NodeData::Fragment) { p.doc.children(ch).to_vec() } else { vec![ch] };
            let ok = p.doc.append_child(pa, ch);
            if ok {
                let owner = p.doc.owner_doc(pa);
                for &k in &kids {
                    p.doc.set_owner_tree(k, owner);
                }
                after_insert(p, &kids);
                style_node_changed(p, pa);
            }
            ok
        })
        .unwrap_or(false),
        _ => false,
    };
    Ok(HostValue::from(ok))
});

native!(n_insert_before, |a| {
    let parent = nid(a, 0);
    let child = nid(a, 1);
    let reference = nid(a, 2);
    let ok = match (parent, child) {
        (Some(pa), Some(ch)) => with_page(|p| {
            let kids: Vec<NodeId> = if matches!(p.doc.node(ch).data, NodeData::Fragment) { p.doc.children(ch).to_vec() } else { vec![ch] };
            let ok = p.doc.insert_before(pa, ch, reference);
            if ok {
                let owner = p.doc.owner_doc(pa);
                for &k in &kids {
                    p.doc.set_owner_tree(k, owner);
                }
                after_insert(p, &kids);
            }
            ok
        })
        .unwrap_or(false),
        _ => false,
    };
    Ok(HostValue::from(ok))
});

native!(n_remove, |a| {
    if let Some(id) = nid(a, 0) {
        with_page(|p| {
            if p.doc.focused.map(|f| p.doc.contains(id, f)).unwrap_or(false) {
                p.doc.focused = None;
            }
            p.doc.detach(id);
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_clone, |a| {
    let id = nid(a, 0);
    let deep = boolean(a, 1, false);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.clone_node(id, deep)))))
});

native!(n_template_content, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.template_content(id)))))
});

native!(n_inner_html, |a| {
    let id = nid(a, 0);
    Ok(jstr(&id.and_then(|id| with_page(|p| p.doc.inner_html(id))).unwrap_or_default()))
});

native!(n_get_html, |a| {
    // (node, serializableShadowRoots, "id,id,…" of shadow roots to include)
    let id = nid(a, 0);
    let ser = boolean(a, 1, false);
    let list: Vec<usize> = st(a, 2).split(',').filter_map(|x| x.trim().parse().ok()).collect();
    Ok(jstr(&id.and_then(|id| with_page(|p| p.doc.inner_html_with(id, &|root, info| (ser && info.serializable) || list.contains(&root)))).unwrap_or_default()))
});

native!(n_attach_declarative, |a| {
    // Declarative shadow roots in a subtree parsed by setHTMLUnsafe()/parseHTMLUnsafe().
    if let Some(id) = nid(a, 0) {
        with_page(|p| p.doc.attach_declarative_shadows(id));
    }
    Ok(HostValue::Null)
});

native!(n_outer_html, |a| {
    let id = nid(a, 0);
    Ok(jstr(&id.and_then(|id| with_page(|p| p.doc.outer_html(id))).unwrap_or_default()))
});

native!(n_set_inner_html, |a| {
    let id = nid(a, 0);
    let html = st(a, 1);
    if let Some(id) = id {
        with_page(|p| {
            // A template's innerHTML goes into its contents.
            let id = if p.doc.is_tag(id, "template") { p.doc.template_content(id) } else { id };
            p.doc.remove_all_children(id);
            let frag = crate::html::parse_fragment(&mut p.doc, id, &html);
            let kids = p.doc.children(frag).to_vec();
            p.doc.append_child(id, frag);
            // Scripts inserted via innerHTML don't execute (not even when moved
            // later: they count as already started); stylesheets do apply.
            for k in kids {
                let mut all = vec![k];
                all.extend(p.doc.descendants(k));
                for d in all {
                    if p.doc.is_tag(d, "script") {
                        p.doc.set_attr(d, "data-ohb-inert", "");
                    }
                    if p.doc.is_tag(d, "style") && p.doc.is_connected(d) {
                        p.add_sheet_node(d, false);
                    }
                    if p.doc.is_tag(d, "link") && p.doc.is_connected(d) && p.doc.attr(d, "rel").map(|r| r.contains("stylesheet")).unwrap_or(false) {
                        p.add_sheet_node(d, true);
                    }
                }
            }
            style_node_changed(p, id);
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_parse_fragment, |a| {
    // Parse HTML into a detached fragment (for insertAdjacentHTML / DOMParser / template).
    let ctx_id = nid(a, 0).unwrap_or(DOCUMENT);
    let html = st(a, 1);
    let run_scripts = boolean(a, 2, false);
    let id = with_page(|p| {
        let ctx_el = if p.doc.is_element(ctx_id) { ctx_id } else { p.doc.body().unwrap_or(ctx_id) };
        let f = crate::html::parse_fragment(&mut p.doc, ctx_el, &html);
        if !run_scripts {
            // Mark scripts as already started so later insertion doesn't run them.
            let scripts: Vec<NodeId> = p.doc.descendants(f).filter(|&d| p.doc.is_tag(d, "script")).collect();
            for s in scripts {
                p.doc.set_attr(s, "data-ohb-inert", "");
            }
        }
        f
    });
    Ok(jid(id))
});

/// Selector lists parsed for querySelector/matches/closest (scripts query the
/// same few selectors over and over).
fn cached_selector(sel: &str) -> Option<std::rc::Rc<Vec<crate::css::selector::Selector>>> {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<String, Option<std::rc::Rc<Vec<crate::css::selector::Selector>>>>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    if let Some(v) = CACHE.with(|c| c.borrow().get(sel).cloned()) {
        return v;
    }
    let v = crate::css::selector::parse_selector_list_str(sel).map(std::rc::Rc::new);
    if sel.len() < 512 {
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() > 2048 {
                c.clear();
            }
            c.insert(sel.to_string(), v.clone());
        });
    }
    v
}

native!(n_query, |a| {
    let root = nid(a, 0).unwrap_or(DOCUMENT);
    let sel = st(a, 1);
    let all = boolean(a, 2, false);
    let parsed = cached_selector(&sel);
    match parsed {
        None => Ok(HostValue::Null),
        Some(list) => {
            let v = with_page(|p| crate::css::selector::query_all(&p.doc, root, &list, !all)).unwrap_or_default();
            Ok(jids(v))
        }
    }
});

native!(n_matches, |a| {
    let id = nid(a, 0);
    let sel = st(a, 1);
    let parsed = cached_selector(&sel);
    match (id, parsed) {
        (_, None) => Ok(HostValue::Null),
        (Some(id), Some(list)) => {
            let r = with_page(|p| {
                let hov = p.hover_chain.clone();
                let st = crate::css::selector::MatchState { hover_chain: &hov, focus: p.doc.focused, active_chain: &[], target: None, scope: Some(id), host: None, shadow_root: None };
                crate::css::selector::matches_any(&list, &p.doc, id, &st)
            })
            .unwrap_or(false);
            Ok(HostValue::from(r))
        }
        _ => Ok(HostValue::from(false)),
    }
});

native!(n_closest, |a| {
    let id = nid(a, 0);
    let sel = st(a, 1);
    let parsed = cached_selector(&sel);
    match (id, parsed) {
        (_, None) => Ok(HostValue::Null),
        (Some(id), Some(list)) => {
            let r = with_page(|p| {
                let hov = p.hover_chain.clone();
                let st = crate::css::selector::MatchState { hover_chain: &hov, focus: p.doc.focused, active_chain: &[], target: None, scope: Some(id), host: None, shadow_root: None };
                let mut cur = Some(id);
                while let Some(n) = cur {
                    if p.doc.is_element(n) && crate::css::selector::matches_any(&list, &p.doc, n, &st) {
                        return Some(n);
                    }
                    cur = p.doc.parent_element(n);
                }
                None
            })
            .flatten();
            Ok(jid(r))
        }
        _ => Ok(jid(None)),
    }
});

native!(n_by_id, |a| {
    let s = st(a, 0);
    let root = nid(a, 1).unwrap_or(DOCUMENT);
    Ok(jid(with_page(|p| if root == DOCUMENT { p.doc.get_element_by_id(&s) } else { p.doc.get_element_by_id_in(root, &s) }).flatten()))
});

// Elements a window named property `name` refers to (tree order).
native!(n_named_props, |a| {
    // (name, document?) -> the elements a window's named property finds.
    let s = st(a, 0);
    let root = nid(a, 1).unwrap_or(DOCUMENT);
    Ok(jids(with_page(|p| p.doc.id_index_of(root).named.get(&s).cloned().unwrap_or_default()).unwrap_or_default()))
});

native!(n_by_tag, |a| {
    // (root, qualifiedName, inHtmlDocument)
    let root = nid(a, 0).unwrap_or(DOCUMENT);
    let t = st(a, 1);
    let html = boolean(a, 2, true);
    let v = with_page(|p| p.doc.elements_by_qualified_name(root, &t, html)).unwrap_or_default();
    Ok(jids(v))
});

native!(n_by_class, |a| {
    let root = nid(a, 0).unwrap_or(DOCUMENT);
    let names: Vec<String> = st(a, 1).split_whitespace().map(|s| s.to_string()).collect();
    let v = with_page(|p| {
        p.doc
            .descendants(root)
            .filter(|&n| p.doc.element(n).map(|e| !names.is_empty() && names.iter().all(|c| e.has_class(c))).unwrap_or(false))
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    Ok(jids(v))
});

native!(n_contains, |a| {
    let x = nid(a, 0);
    let y = nid(a, 1);
    Ok(HostValue::from(match (x, y) {
        (Some(x), Some(y)) => with_page(|p| p.doc.contains(x, y)).unwrap_or(false),
        _ => false,
    }))
});

// ------------------------------------------------------------ shadow DOM

native!(n_attach_shadow, |a| {
    let host = nid(a, 0);
    let open = boolean(a, 1, true);
    let delegates_focus = boolean(a, 2, false);
    let manual = boolean(a, 3, false);
    let clonable = boolean(a, 4, false);
    let serializable = boolean(a, 5, false);
    Ok(jid(host.and_then(|h| {
        with_page(|p| {
            // A declarative shadow root is replaced (emptied) by the first attachShadow().
            if let Some(r) = p.doc.shadow_root(h) {
                let decl = p.doc.shadow_info.get(&r).map(|i| i.declarative).unwrap_or(false);
                if decl {
                    p.doc.remove_all_children(r);
                    if let Some(i) = p.doc.shadow_info.get_mut(&r) {
                        i.declarative = false;
                    }
                    return Some(r);
                }
                return None;
            }
            let info = crate::dom::ShadowInfo { host: h, open, delegates_focus, manual, clonable, serializable, declarative: false };
            p.doc.attach_shadow(h, info)
        })
        .flatten()
    })))
});

native!(n_shadow_root, |a| {
    let host = nid(a, 0);
    Ok(jid(host.and_then(|h| with_page(|p| p.doc.shadow_root(h)).flatten())))
});

// [host, open, delegatesFocus, manual, clonable, serializable] of a shadow root, or null.
native!(n_shadow_info, |a| {
    let root = nid(a, 0);
    let v = root.and_then(|r| with_page(|p| p.doc.shadow_info.get(&r).cloned()).flatten());
    Ok(match v {
        Some(i) => HostValue::Array(vec![
            HostValue::Number(i.host as f64),
            HostValue::from(i.open),
            HostValue::from(i.delegates_focus),
            HostValue::from(i.manual),
            HostValue::from(i.clonable),
            HostValue::from(i.serializable),
        ]),
        None => HostValue::Null,
    })
});

native!(n_assigned_nodes, |a| {
    let slot = nid(a, 0);
    Ok(jids(slot.and_then(|s| with_page(|p| p.doc.slot_map().assigned.get(&s).cloned().unwrap_or_default())).unwrap_or_default()))
});

native!(n_assigned_slot, |a| {
    let n = nid(a, 0);
    Ok(jid(n.and_then(|n| with_page(|p| p.doc.assigned_slot(n)).flatten())))
});

// slot.assign(...nodes) for manual slot assignment.
native!(n_slot_assign, |a| {
    let slot = nid(a, 0);
    let v = a.value(1)?;
    let ids: Vec<NodeId> = match v {
        HostValue::Array(v) => v.iter().map(|x| x.number()).filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as usize).collect(),
        _ => Vec::new(),
    };
    if let Some(s) = slot {
        with_page(|p| {
            let ids: Vec<NodeId> = ids.into_iter().filter(|&i| p.doc.valid(i)).collect();
            // A node is given to one slot at a time.
            for list in p.doc.manual_slots.values_mut() {
                list.retain(|n| !ids.contains(n));
            }
            p.doc.manual_slots.insert(s, ids);
        });
    }
    Ok(HostValue::Undefined)
});

// Constructed sheets adopted by the document or a shadow root (their CSS texts).
native!(n_set_adopted, |a| {
    let root = nid(a, 0);
    let v = a.value(1)?;
    let texts: Vec<std::rc::Rc<str>> = match v {
        HostValue::Array(v) => v.iter().map(|x| std::rc::Rc::from(x.string().as_str())).collect(),
        _ => Vec::new(),
    };
    if let Some(r) = root {
        with_page(|p| {
            if texts.is_empty() {
                p.doc.adopted.remove(&r);
            } else {
                p.doc.adopted.insert(r, texts);
            }
            if p.doc.is_shadow_root(r) {
                // Only that tree's rule index changes (rebuilt each style
                // pass; the incremental restyle covers its host).
                p.need_style = true;
                p.need_layout = true;
            } else {
                p.restyle_sheets();
            }
        });
    }
    Ok(HostValue::Undefined)
});

// Custom element candidates (names with a dash, or an `is` attribute) among the
// shadow-including inclusive descendants of a node, in tree order.
native!(n_ce_walk, |a| {
    let id = nid(a, 0);
    Ok(jids(id.and_then(|id| {
        with_page(|p| {
            let is_ce = |n: NodeId| p.doc.element(n).map(|e| e.ns == crate::dom::Namespace::Html && (e.name.contains('-') || e.has_attr("is"))).unwrap_or(false);
            let mut out = Vec::new();
            if is_ce(id) {
                out.push(id);
            }
            if p.doc.shadow_roots.is_empty() {
                out.extend(p.doc.descendants(id).filter(|&d| is_ce(d)));
            } else {
                out.extend(p.doc.shadow_including_descendants(id).into_iter().filter(|&d| is_ce(d)));
            }
            out
        })
    }).unwrap_or_default()))
});

native!(n_shadow_count, |a| {
    Ok(HostValue::Number(with_page(|p| p.doc.shadow_info.len()).unwrap_or(0) as f64))
});

// Is `a` a shadow-including inclusive ancestor of `b`?
native!(n_s_contains, |a| {
    let x = nid(a, 0);
    let y = nid(a, 1);
    Ok(HostValue::from(match (x, y) {
        (Some(x), Some(y)) => with_page(|p| p.doc.shadow_including_contains(x, y)).unwrap_or(false),
        _ => false,
    }))
});

// [slot, n, ids…]* — the assigned nodes of every slot that has any.
native!(n_slot_snapshot, |a| {
    let v = with_page(|p| {
        let m = p.doc.slot_map();
        let mut slots: Vec<(&NodeId, &Vec<NodeId>)> = m.assigned.iter().collect();
        slots.sort_unstable_by_key(|(s, _)| **s);
        let mut out = Vec::new();
        for (s, list) in slots {
            out.push(*s);
            out.push(list.len());
            out.extend(list.iter().copied());
        }
        out
    })
    .unwrap_or_default();
    Ok(jids(v))
});

// A custom element finished construction (it now matches :defined).
native!(n_ce_defined, |a| {
    if let Some(id) = nid(a, 0) {
        with_page(|p| {
            if let Some(e) = p.doc.element_mut(id) {
                e.ce_defined = true;
            }
            p.doc.touch(crate::dom::StyleRecord::State(id));
        });
    }
    Ok(HostValue::Undefined)
});

// An inserted inline script ran synchronously: it is not run again later.
native!(n_script_started, |a| {
    if let Some(id) = nid(a, 0) {
        with_page(|p| {
            for s in p.scripts.iter_mut().filter(|s| s.node == id) {
                s.done = true;
                s.text = None;
            }
        });
    }
    Ok(HostValue::Undefined)
});

// A <script> the page's loader already ran (a parser-inserted one): moving
// it must not run it again.
native!(n_script_known, |a| {
    let id = nid(a, 0);
    Ok(HostValue::from(id.and_then(|id| with_page(|p| p.scripts.iter().any(|s| s.node == id && s.done))).unwrap_or(false)))
});

// (id) -> whether the script changed document since (id, true) last cleared it.
native!(n_script_moved, |a| {
    let id = nid(a, 0);
    let clear = a.len() > 1;
    Ok(HostValue::from(id.and_then(|id| with_page(|p| if clear { p.doc.moved_scripts.remove(&id) } else { p.doc.moved_scripts.contains(&id) })).unwrap_or(false)))
});

native!(n_coi_sw, |a| {
    // (op, scriptURL, scope): "register" a coi-serviceworker / "scope" the
    // scope controlling this document (or null).
    let op = st(a, 0);
    let doc_url = with_page(|p| p.doc.url.to_string()).unwrap_or_default();
    match op.as_str() {
        "register" => {
            let (script, scope) = (st(a, 1), st(a, 2));
            let same_origin = url::Url::parse(&script).ok().zip(url::Url::parse(&doc_url).ok()).is_some_and(|(s, d)| s.origin() == d.origin());
            let name = script.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
            if same_origin && name.starts_with("coi-serviceworker") && scope.starts_with("http") {
                crate::page::coi_register(&scope);
                return Ok(HostValue::from(true));
            }
            Ok(HostValue::from(false))
        }
        _ => Ok(crate::page::coi_scope_of(&doc_url).map(|s| jstr(&s)).unwrap_or(HostValue::Null)),
    }
});

native!(n_cross_origin_isolated, |_a| {
    Ok(HostValue::from(with_page(|p| p.cross_origin_isolated).unwrap_or(false)))
});

native!(n_composed_root, |a| {
    let id = nid(a, 0);
    Ok(jid(id.and_then(|id| with_page(|p| p.doc.shadow_including_root(id)))))
});

// Connected to any document (shadow-including root is a Document): custom
// element reactions fire in documents without a browsing context too.
native!(n_connected_any, |a| {
    let x = nid(a, 0);
    Ok(HostValue::from(x.and_then(|x| with_page(|p| {
        let mut n = x;
        for _ in 0..100_000 {
            if p.doc.is_document(n) {
                return true;
            }
            match p.doc.parent(n).or_else(|| p.doc.host_of(n)) {
                Some(q) => n = q,
                None => return false,
            }
        }
        false
    })).unwrap_or(false)))
});

native!(n_connected, |a| {
    let x = nid(a, 0);
    Ok(HostValue::from(x.and_then(|x| with_page(|p| p.doc.is_connected(x))).unwrap_or(false)))
});

native!(n_value, |a| {
    let id = nid(a, 0);
    Ok(jstr(&id.and_then(|id| with_page(|p| p.control_value(id))).unwrap_or_default()))
});

native!(n_set_value, |a| {
    let id = nid(a, 0);
    let v = st(a, 1);
    if let Some(id) = id {
        with_page(|p| {
            if p.doc.is_tag(id, "select") {
                let opts: Vec<NodeId> = p.doc.descendants(id).filter(|&o| p.doc.is_tag(o, "option")).collect();
                for o in opts {
                    let ov = p.doc.attr(o, "value").map(|s| s.to_string()).unwrap_or_else(|| crate::dom::collapse_ws(&p.doc.text_content(o)));
                    if let Some(e) = p.doc.element_mut(o) {
                        e.selected = Some(ov == v);
                    }
                }
                p.doc.control_changed(id);
            } else {
                p.set_control_value(id, &v);
                if p.doc.focused == Some(id) {
                    p.caret = v.len();
                }
            }
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_form_state, |a| {
    // (control, op, arg?): form-control state kept with the element.
    //   "dirty" -> whether the value is dirty; "clearValue" / "clearChecked" /
    //   "clearSelected" drop the dirty state (back to the content attributes);
    //   "indeterminate" [bool]; "customValidity" [message].
    let id = nid(a, 0);
    let op = st(a, 1);
    let Some(id) = id else { return Ok(HostValue::Undefined) };
    let arg_given = a.len() > 2;
    let b = boolean(a, 2, false);
    let msg = opt_st(a, 2);
    Ok(with_page(|p| {
        let mut changed = false;
        let r = match p.doc.element_mut(id) {
            None => HostValue::Undefined,
            Some(e) => match op.as_str() {
                "dirty" => HostValue::Bool(e.value.is_some()),
                "dirtyChecked" => HostValue::Bool(e.checked.is_some()),
                "storedSelected" => match e.selected { Some(v) => HostValue::Bool(v), None => HostValue::Undefined },
                "clearValue" => { changed = e.value.take().is_some(); HostValue::Undefined }
                "clearChecked" => { changed = e.checked.take().is_some(); HostValue::Undefined }
                "clearSelected" => { changed = e.selected.take().is_some(); HostValue::Undefined }
                "indeterminate" => {
                    if arg_given && e.indeterminate != b {
                        e.indeterminate = b;
                        changed = true;
                    }
                    HostValue::Bool(e.indeterminate)
                }
                "customValidity" => {
                    let m = msg.filter(|m| !m.is_empty());
                    if e.custom_validity != m {
                        e.custom_validity = m;
                        changed = true;
                    }
                    HostValue::Undefined
                }
                _ => HostValue::Undefined,
            },
        };
        if changed {
            p.doc.control_changed(id);
            p.need_style = true;
            p.need_paint = true;
        }
        r
    })
    .unwrap_or(HostValue::Undefined))
});

// CSS color -> [r, g, b, a] in 0..1 (null when not a color).
native!(n_css_color, |a| {
    let s = st(a, 0);
    let cvs = crate::css::tokenizer::parse_cvs(&s);
    let parts: Vec<_> = cvs.iter().filter(|c| !c.is_ws()).collect();
    if parts.len() != 1 {
        return Ok(HostValue::Null);
    }
    match crate::css::values::parse_color(parts[0]) {
        Some(crate::css::values::ColorValue::Rgba(c)) => Ok(HostValue::Array(vec![
            HostValue::from(c.r as f64 / 255.0),
            HostValue::from(c.g as f64 / 255.0),
            HostValue::from(c.b as f64 / 255.0),
            HostValue::from(c.a as f64 / 255.0),
        ])),
        _ => Ok(HostValue::Null),
    }
});

native!(n_checked, |a| {
    let id = nid(a, 0);
    Ok(HostValue::from(id.and_then(|id| with_page(|p| p.doc.element(id).map(|e| e.checked.unwrap_or_else(|| e.has_attr("checked"))))).flatten().unwrap_or(false)))
});

native!(n_set_checked, |a| {
    let id = nid(a, 0);
    let v = boolean(a, 1, false);
    if let Some(id) = id {
        with_page(|p| p.set_checked(id, v));
    }
    Ok(HostValue::Undefined)
});

native!(n_selected, |a| {
    let id = nid(a, 0);
    Ok(HostValue::from(id.and_then(|id| with_page(|p| p.doc.element(id).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))))).flatten().unwrap_or(false)))
});

native!(n_set_selected, |a| {
    // An option's selectedness (script keeps single-selects exclusive).
    let id = nid(a, 0);
    let v = boolean(a, 1, false);
    if let Some(id) = id {
        with_page(|p| {
            if let Some(e) = p.doc.element_mut(id) {
                e.selected = Some(v);
            }
            p.doc.control_changed(id);
            p.need_style = true;
            p.need_paint = true;
        });
    }
    Ok(HostValue::Undefined)
});

// A string in a form's encoding (accept-charset, else the document's), with
// unencodable characters as decimal character references (multipart bodies).
native!(n_form_charset_encode, |a| {
    let id = nid(a, 0);
    let text = st(a, 1);
    let bytes = with_page(|p| {
        let doc = &p.doc;
        let charset: &'static str = id
            .and_then(|f| doc.attr(f, "accept-charset"))
            .and_then(|a| a.split_ascii_whitespace().find_map(crate::html::encoding::canonical_name))
            .unwrap_or(doc.charset);
        if charset == "UTF-8" {
            text.as_bytes().to_vec()
        } else {
            crate::html::encoding::encode_with(charset, &text, &|c, out: &mut Vec<u8>| out.extend_from_slice(format!("&#{};", c as u32).as_bytes()))
        }
    })
    .unwrap_or_default();
    Ok(HostValue::Bytes(bytes))
});

// WebSockets: (id, url, protocols[]) opens; (id, kind 0 text / 1 bytes, data)
// sends; (id, code|null, reason) closes.
native!(n_ws_open, |a| {
    let id = num(a, 0) as u64;
    let url = st(a, 1);
    let protocols: Vec<String> = a.value(2).ok().and_then(|v| v.as_array().map(|l| l.iter().map(|x| x.string()).collect())).unwrap_or_default();
    let Ok(u) = url::Url::parse(&url) else { return Ok(HostValue::Bool(false)) };
    with_page(|p| {
        let (tx, rx) = std::sync::mpsc::channel();
        p.ws_conns.insert(id, tx);
        let ptx = p.tx.clone();
        let origin = p.doc.url.origin().ascii_serialization();
        crate::net::ws::connect(u, protocols, origin, rx, move |ev| {
            let _ = ptx.send(PageMsg::Ws(id, ev));
        });
    });
    Ok(HostValue::Bool(true))
});

native!(n_ws_send, |a| {
    let id = num(a, 0) as u64;
    let cmd = match a.value(2).unwrap_or(HostValue::Undefined) {
        HostValue::Bytes(b) => crate::net::ws::WsCmd::Binary(b),
        v => crate::net::ws::WsCmd::Text(v.string()),
    };
    with_page(|p| {
        if let Some(tx) = p.ws_conns.get(&id) {
            let _ = tx.send(cmd);
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_ws_close, |a| {
    let id = num(a, 0) as u64;
    let c = num(a, 1);
    let code = if c.is_finite() { Some(c as u16) } else { None };
    let reason = opt_st(a, 2).unwrap_or_default();
    with_page(|p| {
        if let Some(tx) = p.ws_conns.get(&id) {
            let _ = tx.send(crate::net::ws::WsCmd::Close(code, reason));
        }
    });
    Ok(HostValue::Undefined)
});

// The focused element (-1 when none: activeElement is then the body).
native!(n_focused, |_a| { Ok(HostValue::from(with_page(|p| p.doc.focused.map(|f| f as f64)).flatten().unwrap_or(-1.0))) });

// Whether the element is being rendered (has a box), for focusability.
native!(n_focus_rendered, |a| {
    let id = nid(a, 0);
    Ok(HostValue::from(
        id.and_then(|id| {
            with_page(|p| {
                // (Local frames lay out when they render: their elements count as rendered.)
                if p.doc.tree_document(id) != crate::dom::DOCUMENT {
                    return Some(true);
                }
                p.ensure_layout();
                p.layout.as_ref().map(|t| t.node_box.contains_key(&id))
            })
            .flatten()
        })
        .unwrap_or(false),
    ))
});

native!(n_focus, |a| {
    let id = nid(a, 0);
    with_page(|p| p.focus(id));
    Ok(HostValue::Undefined)
});

native!(n_active, |a| { Ok(jid(with_page(|p| p.doc.focused.or(p.doc.body())).flatten())) });

native!(n_caret, |a| {
    // (end[, start]) in UTF-16 code units of the focused control's value (the
    // caret itself is a byte offset); with a start, the range between is
    // selected. NaN reads the caret.
    let set = num(a, 0);
    let start = if a.len() > 1 { num(a, 1) } else { f64::NAN };
    let r = with_page(|p| {
        let v = p.doc.focused.map(|f| p.control_value(f)).unwrap_or_default();
        let to_byte = |want: usize| {
            let mut units = 0usize;
            for (i, ch) in v.char_indices() {
                if units >= want {
                    return i;
                }
                units += ch.len_utf16();
            }
            v.len()
        };
        if set.is_finite() {
            p.caret = to_byte(set.max(0.0) as usize);
            p.field_anchor = if start.is_finite() { Some(to_byte(start.max(0.0) as usize)).filter(|&s| s != p.caret) } else { None };
            p.need_paint = true;
        }
        let c = p.caret.min(v.len());
        v.char_indices().take_while(|(i, _)| *i < c).map(|(_, ch)| ch.len_utf16()).sum::<usize>()
    });
    Ok(HostValue::from(r.unwrap_or(0) as f64))
});

// ---------------------------------------------------------- layout natives

native!(n_rect, |a| {
    let id = nid(a, 0);
    let r = id.and_then(|id| {
        with_page(|p| {
            p.frame_ctx(id, |p| {
                let r = p.node_rect(id)?;
                // Fixed-position boxes are laid out in viewport coordinates.
                let (sx, sy) = if p.node_in_fixed(id) { (0.0, 0.0) } else { p.scroll };
                Some([r.x as f64 - sx as f64, r.y as f64 - sy as f64, r.w as f64, r.h as f64])
            })
        })
        .flatten()
    });
    let v = r.unwrap_or([0.0; 4]);
    Ok(jnums(&v))
});

native!(n_client_rects, |a| {
    // getClientRects(): [x, y, w, h, ...] in viewport coordinates.
    let id = nid(a, 0);
    let v: Vec<f64> = id
        .and_then(|id| {
            with_page(|p| {
                p.frame_ctx(id, |p| {
                    let rs = p.node_client_rects(id);
                    let (sx, sy) = if p.node_in_fixed(id) { (0.0, 0.0) } else { p.scroll };
                    rs.iter().flat_map(|r| [r.x as f64 - sx as f64, r.y as f64 - sy as f64, r.w as f64, r.h as f64]).collect::<Vec<f64>>()
                })
            })
        })
        .unwrap_or_default();
    Ok(jnums(&v))
});

native!(n_text_range_rects, |a| {
    // Client rects of a Text node's characters [s, e): [x, y, w, h, ...] (viewport).
    let id = nid(a, 0);
    let (st, en) = (num(a, 1).max(0.0) as usize, num(a, 2).max(0.0) as usize);
    let v: Vec<f64> = id
        .and_then(|id| {
            with_page(|p| {
                p.frame_ctx(id, |p| {
                    p.ensure_layout();
                    let (sx, sy) = if p.node_in_fixed(id) { (0.0, 0.0) } else { p.scroll };
                    let tree = p.layout.as_ref()?;
                    let rs = tree.text_range_rects(&p.doc, id, st, en);
                    Some(rs.iter().flat_map(|r| [(r.0 - sx) as f64, (r.1 - sy) as f64, r.2 as f64, r.3 as f64]).collect::<Vec<f64>>())
                })
            })
            .flatten()
        })
        .unwrap_or_default();
    Ok(jnums(&v))
});

native!(n_caret_from_point, |a| {
    // caretPositionFromPoint: [textNode, offset] at viewport point (x, y) of
    // the document `doc` (a frame's or the page's), or null.
    let doc = nid(a, 0).unwrap_or(0);
    let (x, y) = (num(a, 1) as f32, num(a, 2) as f32);
    let r = with_page(|p| {
        p.frame_ctx(doc, |p| {
            p.ensure_layout();
            let (sx, sy) = p.scroll;
            let tree = p.layout.as_ref()?;
            let pt = tree.text_hit(x + sx, y + sy, true).or_else(|| tree.text_hit(x + sx, y + sy, false))?;
            tree.point_to_dom(&p.doc, pt)
        })
    })
    .flatten();
    Ok(match r {
        Some((n, off)) => HostValue::Array(vec![HostValue::Number(n as f64), HostValue::Number(off as f64)]),
        None => HostValue::Null,
    })
});

native!(n_layout_rect, |a| {
    // [x, y, w, h, fixed] of the border box as laid out (document coordinates, untransformed).
    let id = nid(a, 0);
    let r = id.and_then(|id| with_page(|p| p.frame_ctx(id, |p| p.layout_rect(id))).flatten());
    Ok(match r {
        Some((r, fixed)) => jnums(&[r.x as f64, r.y as f64, r.w as f64, r.h as f64, if fixed { 1.0 } else { 0.0 }]),
        None => jnums(&[0.0, 0.0, 0.0, 0.0, 0.0]),
    })
});

native!(n_ro_sizes, |a| {
    // ResizeObserver sizes, untransformed: [borderW, borderH, contentW, contentH,
    // paddingLeft, paddingTop, verticalWritingMode] or null without a box.
    let id = nid(a, 0);
    let v = id.and_then(|id| {
        with_page(|p| {
            p.frame_ctx(id, |p| {
                p.ensure_layout();
                let tree = p.layout.as_ref()?;
                let b = *tree.node_box.get(&id)?;
                let bx = &tree.boxes[b];
                let cw = (bx.w - bx.border[1] - bx.border[3] - bx.padding[1] - bx.padding[3]).max(0.0);
                let ch = (bx.h - bx.border[0] - bx.border[2] - bx.padding[0] - bx.padding[2]).max(0.0);
                let vertical = bx.style.writing_mode != 0;
                Some([bx.w as f64, bx.h as f64, cw as f64, ch as f64, bx.padding[3] as f64, bx.padding[0] as f64, if vertical { 1.0 } else { 0.0 }])
            })
        })
        .flatten()
    });
    Ok(match v {
        Some(v) => jnums(&v),
        None => HostValue::Null,
    })
});

native!(n_box_quad, |a| {
    // [p1x, p1y, p2x, p2y, p3x, p3y, p4x, p4y, width, height] or null.
    let id = nid(a, 0);
    let which = st(a, 1);
    let q = id.and_then(|id| with_page(|p| p.frame_ctx(id, |p| p.node_box_quad(id, &which))).flatten());
    Ok(match q {
        Some((pts, w, h)) => {
            let mut v: Vec<f64> = pts.iter().flat_map(|p| [p.0, p.1]).collect();
            v.push(w as f64);
            v.push(h as f64);
            jnums(&v)
        }
        None => HostValue::Null,
    })
});

native!(n_box_info, |a| {
    // [clientW, clientH, scrollW, scrollH, scrollX, scrollY, borderL, borderT, offsetParent]
    let id = nid(a, 0);
    let v = id
        .and_then(|id| {
            with_page(|p| p.frame_ctx(id, |p| {
                p.ensure_layout();
                let tree = p.layout.as_ref()?;
                let b = *tree.node_box.get(&id)?;
                let bx = &tree.boxes[b];
                let cw = (bx.w - bx.border[1] - bx.border[3]) as f64;
                let ch = (bx.h - bx.border[0] - bx.border[2]) as f64;
                let (sw, sh) = if Some(id) == p.doc.document_element() {
                    ((tree.doc_width as f64).max(cw), (tree.doc_height as f64).max(ch))
                } else {
                    // Scrollable overflow: descendants' border boxes and in-flow content plus end padding.
                    let (ew, eh) = if matches!(bx.kind, crate::layout::BoxKind::Replaced(_)) { (0.0, 0.0) } else { tree.scroll_extent(b) };
                    ((bx.scroll_w as f64).max(cw).max(ew as f64), (bx.scroll_h as f64).max(ch).max(eh as f64))
                };
                let (sx, sy) = if p.in_frame_ctx() && Some(id) == p.doc.document_element() { p.scroll } else { p.elem_scroll.get(&id).copied().unwrap_or((0.0, 0.0)) };
                // offsetParent: nearest positioned ancestor element (or td/th/table, or
                // body) in the flat tree, skipping shadow trees the element is not in;
                // none for fixed-position elements.
                // CSSOM View offsetParent: null for the root and the body; walk
                // the flat tree for a containing block of absolutely positioned
                // boxes (or td/th/table for static boxes, or the body), skipping
                // ancestors in shadow trees the element is not in. A fixed box
                // only has one when an ancestor contains fixed boxes.
                let mut op = None;
                let is_root_or_body = Some(id) == p.doc.document_element() || p.doc.is_tag(id, "body");
                let fixed = bx.style.position == crate::css::style::Position::Fixed;
                let stat = bx.style.position == crate::css::style::Position::Static;
                if !is_root_or_body {
                    let styles = p.styles.as_ref();
                    let mut cur = p.doc.flat_parent_element(id);
                    while let Some(n) = cur {
                        let r = p.doc.root_of(n);
                        let visible = !p.doc.is_shadow_root(r) || p.doc.shadow_including_contains(r, id);
                        let st = styles.and_then(|s| s.get(n));
                        if visible {
                            let hit = match st {
                                Some(st) if fixed => st.is_fixed_cb(),
                                Some(st) => st.is_abs_cb() || p.doc.is_tag(n, "body") || (stat && (p.doc.is_tag(n, "td") || p.doc.is_tag(n, "th") || p.doc.is_tag(n, "table"))),
                                None => false,
                            };
                            if hit {
                                op = Some(n);
                                break;
                            }
                        } else if fixed && st.map(|s| s.position == crate::css::style::Position::Fixed).unwrap_or(false) {
                            break;
                        }
                        cur = p.doc.flat_parent_element(n);
                    }
                }
                Some([cw, ch, sw, sh, sx as f64, sy as f64, bx.border[3] as f64, bx.border[0] as f64, op.map(|x| x as f64).unwrap_or(-1.0)])
            }))
            .flatten()
        })
        .unwrap_or([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0]);
    Ok(jnums(&v))
});

native!(n_set_scroll, |a| {
    let id = nid(a, 0);
    let x = num(a, 1);
    let y = num(a, 2);
    if let Some(id) = id {
        with_page(|p| p.frame_ctx(id, |p| {
            p.ensure_layout();
            if p.doc.body() == Some(id) || p.doc.document_element() == Some(id) {
                let (vw, vh) = p.viewport();
                let (dw, dh) = p.layout.as_ref().map(|l| (l.doc_width, l.doc_height)).unwrap_or((vw, vh));
                let nx = if x.is_finite() { x as f32 } else { p.scroll.0 };
                let ny = if y.is_finite() { y as f32 } else { p.scroll.1 };
                let ns = (nx.clamp(0.0, (dw - vw).max(0.0)), ny.clamp(0.0, (dh - vh).max(0.0)));
                if ns != p.scroll {
                    p.scroll = ns;
                    if p.in_frame_ctx() {
                        // A local frame's viewport: its document gets the scroll event.
                        let d = p.doc.render_root.get();
                        p.note_scrolled(d);
                    } else {
                        p.emit(crate::page::PageEvent::ScrollTo(p.scroll.0, p.scroll.1));
                        p.note_scrolled(crate::dom::DOCUMENT);
                    }
                }
            } else {
                let cur = p.elem_scroll.get(&id).copied().unwrap_or((0.0, 0.0));
                let max = p.elem_scroll_max(id).unwrap_or((0.0, 0.0));
                let nx = if x.is_finite() { x as f32 } else { cur.0 };
                let ny = if y.is_finite() { y as f32 } else { cur.1 };
                let ns = (nx.clamp(0.0, max.0), ny.clamp(0.0, max.1));
                if ns != cur {
                    p.elem_scroll.insert(id, ns);
                    p.need_paint = true;
                    p.note_scrolled(id);
                }
            }
        }));
    }
    Ok(HostValue::Undefined)
});

native!(n_computed_exists, |a| {
    let id = nid(a, 0);
    let pseudo = st(a, 1);
    Ok(HostValue::from(id.and_then(|id| with_page(|p| p.frame_ctx(id, |p| computed_style_for(p, id, &pseudo).is_some()))).unwrap_or(false)))
});

native!(n_computed, |a| {
    let id = nid(a, 0);
    let prop = st(a, 1);
    let pseudo = st(a, 2);
    let v = id.and_then(|id| with_page(|p| p.frame_ctx(id, |p| computed_value(p, id, &prop, &pseudo)))).unwrap_or_default();
    Ok(jstr(&v))
});

/// The used width (per box-sizing) of a block-level element whose width
/// follows from styles alone: it and its ancestors are in-flow blocks in
/// block containers up to the root (no floats, flex, grid, tables, shrink-to-
/// fit or formatting-context roots that floats could narrow). None otherwise.
fn fast_block_width(p: &crate::page::Page, id: NodeId) -> Option<f32> {
    use crate::css::style::*;
    use crate::css::values::Len;
    let styles = p.styles.as_ref()?;
    let doc = &p.doc;
    let html = doc.document_element()?;
    // Content width of `n`'s box, plus its used width per box-sizing.
    fn walk(p: &crate::page::Page, styles: &crate::css::Styles, html: NodeId, n: NodeId, depth: u32) -> Option<(f32, f32)> {
        let doc = &p.doc;
        if depth > 256 {
            return None;
        }
        let s = styles.get(n)?;
        let el = doc.element(n)?;
        if el.ns != crate::dom::Namespace::Html || !matches!(s.display, Display::Block | Display::ListItem) {
            return None;
        }
        if s.float != Float::None || !matches!(s.position, Position::Static | Position::Relative | Position::Sticky) || s.writing_mode != 0 || s.columns.is_some() {
            return None;
        }
        if matches!(el.name.as_str(), "img" | "input" | "textarea" | "select" | "button" | "video" | "canvas" | "iframe" | "object" | "embed" | "audio" | "progress" | "meter" | "fieldset" | "legend" | "table" | "details" | "summary" | "marquee" | "hr") {
            return None;
        }
        if doc.shadow_roots.contains_key(&n) {
            return None;
        }
        // Formatting-context roots beside floats are narrowed (the viewport's
        // own overflow, on the root or body, doesn't count).
        let viewport_overflow = n == html || Some(n) == doc.body();
        if !viewport_overflow && (s.overflow_x != Overflow::Visible || s.overflow_y != Overflow::Visible) {
            return None;
        }
        let cb = if n == html {
            p.viewport().0
        } else {
            let par = doc.parent(n)?;
            if !doc.is_element(par) {
                return None;
            }
            walk(p, styles, html, par, depth + 1)?.0
        };
        let res = |l: &Len| -> Option<f32> {
            match l {
                Len::Px(_) | Len::Pct(_) | Len::Calc(..) | Len::Expr(_) => Some(l.resolve(cb)),
                _ => None,
            }
        };
        let pad = res(&s.padding[1])?.max(0.0) + res(&s.padding[3])?.max(0.0);
        let bor = s.bw(1) + s.bw(3);
        let extra = pad + bor;
        let border_box = s.box_sizing == BoxSizing::BorderBox;
        let to_content = |v: f32| if border_box { (v - extra).max(0.0) } else { v.max(0.0) };
        let mut w = if s.width.is_auto() {
            let m = |l: &Len| if l.is_auto() { Some(0.0) } else { res(l) };
            (cb - m(&s.margin[1])? - m(&s.margin[3])? - extra).max(0.0)
        } else {
            to_content(res(&s.width)?)
        };
        if !s.max_width.is_none() {
            w = w.min(to_content(res(&s.max_width)?));
        }
        if !s.min_width.is_auto() {
            w = w.max(to_content(res(&s.min_width)?));
        }
        let used = if border_box { w + extra } else { w };
        Some((w, used))
    }
    if doc.root_of(id) != crate::dom::DOCUMENT {
        return None;
    }
    walk(p, styles, html, id, 0).map(|(_, used)| used)
}

/// The box of an element's ::before/::after (or the element's own box).
fn pseudo_box(tree: &crate::layout::LayoutTree, b: usize, pseudo: &str) -> Option<usize> {
    let want = if pseudo.contains("before") {
        1
    } else if pseudo.contains("after") {
        2
    } else {
        return Some(b);
    };
    let mut stack: Vec<(usize, u8)> = tree.boxes[b].children.iter().map(|&c| (c, 0)).collect();
    while let Some((c, d)) = stack.pop() {
        let bx = &tree.boxes[c];
        if bx.pseudo == want && bx.node.is_none() {
            return Some(c);
        }
        // Through anonymous wrappers.
        if bx.pseudo == 3 && d < 3 {
            stack.extend(bx.children.iter().map(|&k| (k, d + 1)));
        }
    }
    None
}

fn computed_value(p: &mut crate::page::Page, id: NodeId, prop: &str, pseudo: &str) -> String {
    use crate::css::style::*;
    if p.need_layout && std::env::var_os("OHB_THRASH").is_some() {
        eprintln!("[thrash-prop] {prop}");
    }
    let s = match computed_style_for(p, id, pseudo) {
        Some(s) => s,
        None => {
            return match prop {
                "display" => "none".into(),
                _ => String::new(),
            }
        }
    };
    computed_value_of(p, id, prop, pseudo, s)
}

/// The computed style getComputedStyle reports for an element (None: it has
/// none — detached, outside the flat tree, or in a document that isn't rendered).
fn computed_style_for(p: &mut crate::page::Page, id: NodeId, pseudo: &str) -> Option<std::rc::Rc<crate::css::style::ComputedStyle>> {
    let root = p.doc.root_of(id);
    let sub = root != DOCUMENT && p.doc.is_document(root) && root != p.doc.render_root.get();
    let pick = |styles: &crate::css::Styles| {
        // A pseudo-element without rules of its own inherits from its element.
        let other = |m: &std::collections::HashMap<NodeId, std::rc::Rc<crate::css::style::ComputedStyle>>| {
            m.get(&id).cloned().or_else(|| styles.get(id).map(|e| std::rc::Rc::new(crate::css::style::ComputedStyle::inherit(e))))
        };
        if pseudo.contains("before") {
            styles.before.get(&id).cloned()
        } else if pseudo.contains("after") {
            styles.after.get(&id).cloned()
        } else if pseudo.contains("placeholder") {
            other(&styles.placeholder)
        } else if pseudo.contains("marker") {
            other(&styles.marker)
        } else if pseudo.contains("first-letter") {
            other(&styles.first_letter)
        } else if pseudo.contains("first-line") {
            other(&styles.first_line)
        } else {
            styles.get(id).cloned()
        }
    };
    let mut s = if sub {
        let st = p.subdoc_styles(root);
        pick(&st)
    } else {
        p.ensure_style();
        // Container query units and @container rules depend on layout sizes.
        if !p.in_frame_ctx() && p.index.as_ref().is_some_and(|i| i.has_container_rules) {
            p.ensure_layout();
        }
        match &p.styles {
            Some(s) => pick(s),
            None => return None,
        }
    };
    if s.is_none() && !sub && p.in_frame_ctx() && p.doc.is_element(id) && p.doc.root_of(id) == p.doc.render_root.get() {
        // A local frame's display:none subtree: its document styled in full.
        let st = p.subdoc_styles(p.doc.render_root.get());
        s = pick(&st);
    }
    if s.is_none() && !sub && !p.in_frame_ctx() && p.doc.is_element(id) && p.doc.is_connected(id) {
        // Inside a display:none subtree: style it on demand.
        if let Some((n, b, a)) = p.hidden_style(id) {
            s = if pseudo.contains("before") { b } else if pseudo.contains("after") { a } else { Some(n) };
        }
    }
    s
}

fn computed_value_of(p: &mut crate::page::Page, id: NodeId, prop: &str, pseudo: &str, s: std::rc::Rc<crate::css::style::ComputedStyle>) -> String {
    use crate::css::style::*;
    let root = p.doc.root_of(id);
    let sub = root != DOCUMENT && p.doc.is_document(root) && root != p.doc.render_root.get();
    // Colors with full-precision computed values (lab(), color(), color-mix(), …).
    if let Some(h) = &s.color_hp {
        if let Some((_, spec)) = h.iter().find(|(n, _)| *n == prop) {
            use crate::css::color as cc;
            let legacy = cc::Abs::srgb(s.color.r as f64 / 255.0, s.color.g as f64 / 255.0, s.color.b as f64 / 255.0, s.color.a as f64 / 255.0);
            let (vw, vh) = p.viewport();
            cc::set_unit_env(cc::UnitEnv { em: s.font_size as f64, rem: 16.0, vw: vw as f64, vh: vh as f64, cqw: vw as f64, cqh: vh as f64 });
            let dark = s.dark_scheme;
            let current = match h.iter().find(|(n, _)| *n == "color") {
                Some((_, ct)) if prop != "color" => cc::compute(ct, &legacy, dark).unwrap_or(legacy.clone()),
                _ => legacy.clone(),
            };
            if let Some(out) = cc::compute(spec, &current, dark).map(|c| cc::ser_computed(&c)) {
                return out;
            }
        }
    }
    if prop == "transform-origin" || prop == "perspective-origin" {
        if let Some(out) = resolved_origin(p, id, &s, prop) {
            return out;
        }
    }
    // Properties computed by the property-value engine.
    if let Some(key) = crate::css::pv::engine_routed(prop) {
        if let Some(l) = crate::css::pv::shorthand::longhands(key) {
            let mut map: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
            for n in &l {
                let v = computed_value(p, id, n, pseudo);
                map.insert(n, v);
            }
            let env = engine_env(p, &s);
            return crate::css::pv::shorthand::serialize_computed(key, &|n| map.get(n).cloned(), Box::new(move |n| crate::css::pv::initial_text(n, &env))).unwrap_or_default();
        }
        if let Some(out) = engine_computed(p, &s, key) {
            return out;
        }
    }
    let px = |v: f32| format!("{}px", crate::css::values::fmt(v));
    let col = |c: crate::css::values::Color| c.to_css();
    if let Some(name) = prop.strip_prefix("--") {
        return s.custom.get(&format!("--{name}")).map(|v| crate::css::tokenizer::serialize(v)).unwrap_or_default();
    }
    let layout_size = |p: &mut crate::page::Page, w: bool| -> Option<f32> {
        if sub {
            return None;
        }
        p.ensure_layout();
        let tree = p.layout.as_ref()?;
        let b = pseudo_box(tree, *tree.node_box.get(&id)?, pseudo)?;
        let bx = &tree.boxes[b];
        // Non-replaced inline boxes have no used width/height: the computed value ("auto") shows.
        if matches!(bx.kind, crate::layout::BoxKind::InlineBox) {
            return None;
        }
        // In a rotated (vertical writing-mode) frame the box's width is its physical height.
        let w = if frame_map(tree, b).is_some() { !w } else { w };
        // The used value of width/height is in terms of box-sizing.
        Some(if bx.style.box_sizing == BoxSizing::BorderBox {
            if w { bx.w } else { bx.h }
        } else if w {
            bx.content_w()
        } else {
            bx.content_h()
        })
    };
    // The laid-out box (margins, paddings and insets resolve to used values).
    let used_box = |p: &mut crate::page::Page| -> Option<([f32; 4], [f32; 4], [Option<f32>; 4])> {
        if sub {
            return None;
        }
        p.ensure_layout();
        let tree = p.layout.as_ref()?;
        let b = pseudo_box(tree, *tree.node_box.get(&id)?, pseudo)?;
        let bx = &tree.boxes[b];
        let st = &bx.style;
        let fmap = frame_map(tree, b);
        let mut inset = [None; 4];
        if fmap.is_some() && matches!(st.position, Position::Relative | Position::Sticky) {
            // A box in a rotated frame: the element's physical insets against
            // the parent's physical content box.
            let (mut pw, mut ph) = match bx.parent {
                Some(par) => (tree.boxes[par].content_w(), tree.boxes[par].content_h()),
                None => (tree.viewport.0, tree.viewport.1),
            };
            if bx.parent.and_then(|par| frame_map(tree, par)).is_some() {
                std::mem::swap(&mut pw, &mut ph);
            }
            if st.position == Position::Sticky {
                (pw, ph) = sticky_basis(tree, b);
            }
            for k in 0..4 {
                let basis = if k % 2 == 0 { ph } else { pw };
                inset[k] = s.inset[k].definite(Some(basis));
            }
            if st.position == Position::Relative {
                let given = inset;
                for k in 0..4 {
                    if given[k].is_none() {
                        inset[k] = Some(given[(k + 2) % 4].map(|v| -v).unwrap_or(0.0));
                    }
                }
            }
            let m = fmap.unwrap();
            let un = |a: [f32; 4]| -> [f32; 4] {
                let mut o = [0.0; 4];
                for i in 0..4 {
                    o[m[i]] = a[i];
                }
                o
            };
            return Some((un(bx.margin), un(bx.padding), inset));
        }
        if st.position != Position::Static {
            // The containing block (padding box) of an absolutely positioned
            // box is the one its layout registered it with (the viewport,
            // or e.g. a transformed ancestor for fixed boxes).
            let abs = matches!(st.position, Position::Absolute | Position::Fixed);
            let cbid = if abs { tree.boxes.iter().position(|cb| cb.abs_children.contains(&b)).filter(|&c| tree.boxes[c].parent.is_some()) } else { None };
            let (px0, py0, pw, ph) = match cbid {
                Some(c) => {
                    let cb = &tree.boxes[c];
                    (cb.abs_x + cb.border[3], cb.abs_y + cb.border[0], cb.w - cb.border[1] - cb.border[3], cb.h - cb.border[0] - cb.border[2])
                }
                None if abs => (0.0, 0.0, tree.viewport.0, tree.viewport.1),
                None if st.position == Position::Sticky => {
                    let (w, h) = sticky_basis(tree, b);
                    (0.0, 0.0, w, h)
                }
                None => match bx.parent {
                    Some(par) => (0.0, 0.0, tree.boxes[par].content_w(), tree.boxes[par].content_h()),
                    None => (0.0, 0.0, tree.viewport.0, tree.viewport.1),
                },
            };
            // A box in a rotated frame whose containing block is the frame's
            // (physical) root: its local axes are the root's swapped.
            let (pw, ph) = match cbid {
                Some(c) if fmap.is_some() && tree.boxes[c].style.is_vertical() && tree.boxes[c].parent.map(|pp| !tree.boxes[pp].style.is_vertical()).unwrap_or(true) => (ph, pw),
                _ => (pw, ph),
            };
            for k in 0..4 {
                let basis = if k % 2 == 0 { ph } else { pw };
                inset[k] = st.inset[k].definite(Some(basis));
            }
            if st.position == Position::Relative {
                // An auto side mirrors the opposite one (0 when both are auto).
                let given = inset;
                for k in 0..4 {
                    if given[k].is_none() {
                        inset[k] = Some(given[(k + 2) % 4].map(|v| -v).unwrap_or(0.0));
                    }
                }
            }
            if abs {
                // Used offsets: auto insets become the box's actual position,
                // unless the axis is over-constrained (both insets and the
                // size given): then the specified values stand.
                let top = bx.abs_y - bx.margin[0] - py0;
                let left = bx.abs_x - bx.margin[3] - px0;
                let used = [Some(top), Some(pw - left - bx.w - bx.margin[3] - bx.margin[1]), Some(ph - top - bx.h - bx.margin[0] - bx.margin[2]), Some(left)];
                // (With both insets given, the specified values are the used
                // ones unless over-constrained, where they're what resolves.)
                let over_v = inset[0].is_some() && inset[2].is_some();
                let over_h = inset[1].is_some() && inset[3].is_some();
                for k in 0..4 {
                    let over = if k % 2 == 0 { over_v } else { over_h };
                    if !over {
                        inset[k] = used[k];
                    }
                }
            }
        }
        if let Some(m) = fmap {
            let un = |a: [f32; 4]| -> [f32; 4] {
                let mut o = [0.0; 4];
                for i in 0..4 {
                    o[m[i]] = a[i];
                }
                o
            };
            // (Insets of a box in a rotated frame were found in the frame's
            // own axes, like its margins.)
            let mut pin = [None; 4];
            for i in 0..4 {
                pin[m[i]] = inset[i];
            }
            return Some((un(bx.margin), un(bx.padding), pin));
        }
        Some((bx.margin, bx.padding, inset))
    };
    // (margin-top, border-top-color, …)
    let side = |prop: &str| -> usize {
        if prop.ends_with("top") || prop.contains("-top-") {
            0
        } else if prop.ends_with("right") || prop.contains("-right-") {
            1
        } else if prop.ends_with("bottom") || prop.contains("-bottom-") {
            2
        } else {
            3
        }
    };
    match prop {
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            let k = side(prop);
            return match used_box(p) {
                Some((m, _, _)) => px(m[k]),
                None => px(s.margin[k].resolve(0.0)),
            };
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
            let k = side(prop);
            return match used_box(p) {
                Some((_, pd, _)) => px(pd[k]),
                None => px(s.padding[k].resolve(0.0)),
            };
        }
        "top" | "right" | "bottom" | "left" => {
            let k = side(prop);
            if s.position != Position::Static {
                if let Some((_, _, ins)) = used_box(p) {
                    if let Some(v) = ins[k] {
                        return px(v);
                    }
                }
            }
            return s.inset[k].to_css();
        }
        "flex" => {
            return format!("{} {} {}", crate::css::values::fmt(s.flex_grow), crate::css::values::fmt(s.flex_shrink), s.flex_basis.to_css());
        }
        _ => {}
    }
    match prop {
        "display" => match s.display {
            Display::None => "none",
            Display::Block => "block",
            Display::Inline => "inline",
            Display::InlineBlock => "inline-block",
            Display::Flex => "flex",
            Display::InlineFlex => "inline-flex",
            Display::Grid => "grid",
            Display::InlineGrid => "inline-grid",
            Display::ListItem => "list-item",
            Display::Table => "table",
            Display::InlineTable => "inline-table",
            Display::TableRowGroup => "table-row-group",
            Display::TableHeaderGroup => "table-header-group",
            Display::TableFooterGroup => "table-footer-group",
            Display::TableRow => "table-row",
            Display::TableCell => "table-cell",
            Display::TableCaption => "table-caption",
            Display::TableColumn => "table-column",
            Display::TableColumnGroup => "table-column-group",
            Display::Contents => "contents",
            Display::FlowRoot => "flow-root",
        }
        .into(),
        "caption-side" => if s.caption_bottom { "bottom" } else { "top" }.into(),
        "position" => format!("{:?}", s.position).to_lowercase(),
        "visibility" => format!("{:?}", s.visibility).to_lowercase(),
        "color" => col(s.color),
        "background-color" => col(s.background_color.resolve(s.color)),
        "font-size" => px(s.font_size),
        "font-weight" => s.font_weight.to_string(),
        "font-style" => format!("{:?}", s.font_style).to_lowercase(),
        // Names that aren't a plain identifier sequence serialize quoted.
        "font-family" => s
            .font_family
            .iter()
            .map(|f| {
                let generic = matches!(f.as_str(), "serif" | "sans-serif" | "monospace" | "cursive" | "fantasy" | "system-ui" | "emoji" | "math" | "fangsong" | "ui-serif" | "ui-sans-serif" | "ui-monospace" | "ui-rounded");
                let ident = !f.is_empty() && f.split(' ').all(|w| !w.is_empty() && !w.starts_with(|c: char| c.is_ascii_digit()) && w.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()));
                if generic || (ident && !f.contains(' ')) { f.clone() } else { format!("\"{}\"", f.replace('"', "\\\"")) }
            })
            .collect::<Vec<_>>()
            .join(", "),
        "line-height" => match s.line_height {
            LineHeight::Normal => "normal".into(),
            LineHeight::Number(n) => px(s.font_size * n),
            LineHeight::Px(v) => px(v),
        },
        "opacity" => num6(s.opacity as f64),
        "z-index" => s.z_index.map(|z| z.to_string()).unwrap_or_else(|| "auto".into()),
        "overflow" | "overflow-y" => format!("{:?}", s.overflow_y).to_lowercase(),
        "overflow-x" => format!("{:?}", s.overflow_x).to_lowercase(),
        "float" => format!("{:?}", s.float).to_lowercase(),
        "text-align" => match s.text_align {
            crate::css::style::TextAlign::WebkitCenter => "-webkit-center".to_string(),
            t => format!("{:?}", t).to_lowercase(),
        },
        "white-space" => match s.white_space {
            WhiteSpace::PreWrap => "pre-wrap".into(),
            WhiteSpace::PreLine => "pre-line".into(),
            WhiteSpace::BreakSpaces => "break-spaces".into(),
            w => format!("{:?}", w).to_lowercase(),
        },
        "cursor" => s.cursor.as_str().to_string(),
        "text-transform" => match s.text_transform {
            TextTransform::None => "none".into(),
            TextTransform::MathAuto => "math-auto".into(),
            t => format!("{:?}", t).to_lowercase(),
        },
        "pointer-events" => if s.pointer_events { "auto".into() } else { "none".into() },
        "box-sizing" => if s.box_sizing == BoxSizing::BorderBox { "border-box".into() } else { "content-box".into() },
        "flex-direction" => format!("{:?}", s.flex_direction).to_lowercase(),
        "width" => {
            // Script reading a block's width between DOM writes (jQuery's
            // .width()): when it follows from the styles alone, skip the relayout.
            let fast = if p.need_layout && !sub && pseudo.is_empty() { fast_block_width(p, id) } else { None };
            if fast.is_none() && p.need_layout && std::env::var_os("OHB_THRASH").is_some() {
                let chain: Vec<String> = std::iter::successors(Some(id), |&n| p.doc.parent(n)).take(8).filter(|&n| p.doc.is_element(n)).map(|n| format!("{}.{}[{:?}]", p.doc.tag(n), p.doc.attr(n, "class").unwrap_or("").chars().take(20).collect::<String>(), p.styles.as_ref().and_then(|st| st.get(n)).map(|x| (x.display, x.float, x.position, x.overflow_x))) ).collect();
                eprintln!("[fastw-miss] {}", chain.join(" < "));
            }
            match fast {
                Some(w) if std::env::var_os("OHB_VERIFY_FASTW").is_none() => px(w),
                _ => {
                    let full = layout_size(p, true);
                    if let (Some(a), Some(b)) = (fast, full) {
                        if (a - b).abs() > 0.01 {
                            eprintln!("[fastw] mismatch <{} class={:?}> fast={a} layout={b}", p.doc.tag(id), p.doc.attr(id, "class"));
                        }
                    }
                    full.map(px).unwrap_or_else(|| s.width.to_css())
                }
            }
        }
        "height" => layout_size(p, false).map(px).unwrap_or_else(|| s.height.to_css()),
        "top" => s.inset[0].to_css(),
        "right" => s.inset[1].to_css(),
        "bottom" => s.inset[2].to_css(),
        "left" => s.inset[3].to_css(),
        "margin-top" => px(s.margin[0].resolve(0.0)),
        "margin-right" => px(s.margin[1].resolve(0.0)),
        "margin-bottom" => px(s.margin[2].resolve(0.0)),
        "margin-left" => px(s.margin[3].resolve(0.0)),
        "padding-top" => px(s.padding[0].resolve(0.0)),
        "padding-right" => px(s.padding[1].resolve(0.0)),
        "padding-bottom" => px(s.padding[2].resolve(0.0)),
        "padding-left" => px(s.padding[3].resolve(0.0)),
        "border-top-width" => px(crate::css::style::snap_border_width(s.bw(0))),
        "border-right-width" => px(crate::css::style::snap_border_width(s.bw(1))),
        "border-bottom-width" => px(crate::css::style::snap_border_width(s.bw(2))),
        "border-left-width" => px(crate::css::style::snap_border_width(s.bw(3))),
        "transform" => match &s.transform {
            None => "none".into(),
            Some(ops) => {
                // The matrix of the transform functions alone (transform-origin isn't part of it).
                let (bw, bh) = if sub {
                    (0.0, 0.0)
                } else {
                    p.ensure_layout();
                    p.layout.as_ref().and_then(|t| t.node_box.get(&id).and_then(|&b| pseudo_box(t, b, pseudo)).map(|b| (t.boxes[b].w, t.boxes[b].h))).unwrap_or((0.0, 0.0))
                };
                let m = crate::css::mat4::ops_matrix(ops, bw, bh);
                // Blink stores matrices in floats.
                let f = |v: f64| { let f = v as f32 as f64; if f.abs() < 1e-6 { 0.0 } else { f } };
                if m.is_2d() {
                    format!("matrix({})", m.to_2d().iter().map(|v| num6(f(*v))).collect::<Vec<_>>().join(", "))
                } else {
                    format!("matrix3d({})", m.to_css3d().iter().map(|v| num6(f(*v))).collect::<Vec<_>>().join(", "))
                }
            }
        },
        "translate" => match &s.translate_prop {
            None => "none".into(),
            Some((x, y, z)) => {
                let zero_y = matches!(y, crate::css::values::Len::Px(v) if *v == 0.0);
                if *z != 0.0 {
                    format!("{} {} {}px", x.to_css(), y.to_css(), num6(*z as f64))
                } else if zero_y {
                    x.to_css()
                } else {
                    format!("{} {}", x.to_css(), y.to_css())
                }
            }
        },
        "rotate" => match s.rotate_prop {
            None => "none".into(),
            Some((x, y, z, a)) => {
                let ang = format!("{}deg", num6(a as f64));
                match (x, y, z) {
                    (_, 0.0, 0.0) if x > 0.0 => format!("x {ang}"),
                    (0.0, _, 0.0) if y > 0.0 => format!("y {ang}"),
                    (0.0, 0.0, _) if z > 0.0 => ang,
                    _ => format!("{} {} {} {ang}", num6(x as f64), num6(y as f64), num6(z as f64)),
                }
            }
        },
        "scale" => match s.scale_prop {
            None => "none".into(),
            Some((x, y, z)) => {
                if z != 1.0 {
                    format!("{} {} {}", num6(x as f64), num6(y as f64), num6(z as f64))
                } else if x == y {
                    num6(x as f64)
                } else {
                    format!("{} {}", num6(x as f64), num6(y as f64))
                }
            }
        },
        "content" => match &s.content {
            Some(items) => items.iter().map(|i| match i {
                ContentItem::Str(x) => format!("\"{x}\""),
                _ => String::new(),
            }).collect::<Vec<_>>().join(" "),
            None => "none".into(),
        },
        "min-width" | "min-height" => {
            let l = if prop == "min-width" { &s.min_width } else { &s.min_height };
            // `auto` computes to 0 except on flex and grid items.
            let item = p.doc.parent(id).and_then(|pp| p.styles.as_ref().and_then(|st| st.get(pp))).map(|ps| ps.display.is_flex() || ps.display.is_grid()).unwrap_or(false);
            if l.is_auto() && !item { "0px".into() } else { l.to_css() }
        }
        "max-width" => s.max_width.to_css(),
        "max-height" => s.max_height.to_css(),
        "margin" => four(&s.margin.clone().map(|l| px(l.resolve(0.0)))),
        "padding" => four(&s.padding.clone().map(|l| px(l.resolve(0.0)))),
        "border-width" => four(&[0, 1, 2, 3].map(|i| px(crate::css::style::snap_border_width(s.bw(i))))),
        "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            border_style_css(s.border_style[side(prop)]).into()
        }
        "border-style" => four(&s.border_style.map(|b| border_style_css(b).to_string())),
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => col(s.border_color[side(prop)].resolve(s.color)),
        "border-color" => four(&s.border_color.map(|c| col(c.resolve(s.color)))),
        "border-top-left-radius" | "border-top-right-radius" | "border-bottom-right-radius" | "border-bottom-left-radius" => {
            let i = match prop {
                "border-top-left-radius" => 0,
                "border-top-right-radius" => 1,
                "border-bottom-right-radius" => 2,
                _ => 3,
            };
            radius_css(&s.border_radius[i])
        }
        "border-radius" => four(&s.border_radius.clone().map(|r| radius_css(&r))),
        "outline-style" => border_style_css(s.outline_style).into(),
        "outline-width" => px(if s.outline_style.visible() { crate::css::style::snap_border_width(s.outline_width) } else { 0.0 }),
        "outline-color" => col(s.outline_color.resolve(s.color)),
        "caret-color" => col(s.caret_color.unwrap_or(s.color)),
        "outline-offset" => px(s.outline_offset),
        "flex-wrap" => match s.flex_wrap {
            FlexWrap::NoWrap => "nowrap".into(),
            FlexWrap::Wrap => "wrap".into(),
            FlexWrap::WrapReverse => "wrap-reverse".into(),
        },
        "flex-grow" => crate::css::values::fmt(s.flex_grow),
        "flex-shrink" => crate::css::values::fmt(s.flex_shrink),
        "flex-basis" => s.flex_basis.to_css(),
        "order" => s.order.to_string(),
        "align-items" => align_css(s.align_items, true),
        "align-self" => align_css(s.align_self, true),
        "align-content" => align_css(s.align_content, true),
        "justify-content" => align_css(s.justify_content, true),
        "justify-items" => align_css(s.justify_items, false),
        "justify-self" => align_css(s.justify_self, false),
        "row-gap" => gap_css(&s.row_gap),
        "column-gap" => gap_css(&s.column_gap),
        "gap" => {
            let (r, c) = (gap_css(&s.row_gap), gap_css(&s.column_gap));
            if r == c { r } else { format!("{r} {c}") }
        }
        "vertical-align" => match &s.vertical_align {
            VerticalAlign::Len(l) => l.to_css(),
            VerticalAlign::TextTop => "text-top".into(),
            VerticalAlign::TextBottom => "text-bottom".into(),
            v => format!("{:?}", v).to_lowercase(),
        },
        "letter-spacing" => if s.letter_spacing == 0.0 { "normal".into() } else { px(s.letter_spacing) },
        "word-spacing" => px(s.word_spacing),
        "text-indent" => px(s.text_indent.resolve(0.0)),
        "text-overflow" => if s.text_overflow_ellipsis { "ellipsis".into() } else { "clip".into() },
        "object-fit" => match s.object_fit {
            ObjectFit::ScaleDown => "scale-down".into(),
            o => format!("{:?}", o).to_lowercase(),
        },
        "user-select" | "-webkit-user-select" => if s.user_select { "auto".into() } else { "none".into() },
        "aspect-ratio" => s.aspect_ratio.map(|r| format!("{} / 1", crate::css::values::fmt(r))).unwrap_or_else(|| "auto".into()),
        "box-shadow" => if s.box_shadow.as_ref().map(|v| v.is_empty()).unwrap_or(true) { "none".into() } else {
            s.box_shadow.as_ref().unwrap().iter().map(|sh| format!("{} {} {} {} {}{}", col(sh.color.resolve(s.color)), px(sh.x), px(sh.y), px(sh.blur), px(sh.spread), if sh.inset { " inset" } else { "" })).collect::<Vec<_>>().join(", ")
        },
        "background-image" => match s.background.as_ref() {
            Some(layers) if layers.iter().any(|l| !matches!(l.image, BgImage::None)) => layers
                .iter()
                .map(|l| match &l.image {
                    BgImage::Url(u) => format!("url(\"{u}\")"),
                    BgImage::None => "none".into(),
                    _ => "linear-gradient()".into(),
                })
                .collect::<Vec<_>>()
                .join(", "),
            _ => "none".into(),
        },
        "fill" => s.fill.as_ref().map(|f| svg_paint_css(f, s.color)).unwrap_or_else(|| "rgb(0, 0, 0)".into()),
        "stroke" => s.stroke.as_ref().map(|f| svg_paint_css(f, s.color)).unwrap_or_else(|| "none".into()),
        "text-decoration-line" | "text-decoration" => {
            let l = s.text_decoration_line;
            let mut v = Vec::new();
            if l & 1 != 0 { v.push("underline"); }
            if l & 2 != 0 { v.push("overline"); }
            if l & 4 != 0 { v.push("line-through"); }
            if v.is_empty() { "none".into() } else { v.join(" ") }
        }
        "table-layout" => if s.table_layout_fixed { "fixed".into() } else { "auto".into() },
        "animation-name" | "animation-duration" | "animation-delay" | "animation-timing-function" | "animation-iteration-count" | "animation-direction" | "animation-fill-mode" | "animation-play-state" | "animation" => anim_css(&s, prop),
        "transition-property" | "transition-duration" | "transition-delay" | "transition-timing-function" | "transition-behavior" | "transition" => trans_css(&s, prop),
        "direction" => if s.direction_rtl { "rtl".into() } else { "ltr".into() },
        "writing-mode" => ["horizontal-tb", "vertical-rl", "vertical-lr", "sideways-rl", "sideways-lr"][s.writing_mode.min(4) as usize].into(),
        "text-orientation" => ["mixed", "upright", "sideways"][s.text_orientation.min(2) as usize].into(),
        "text-combine-upright" => if s.text_combine { "all".into() } else { "none".into() },
        "perspective" => s.perspective.map(|d| format!("{}px", num6(d as f64))).unwrap_or_else(|| "none".into()),
        "transform-style" => if s.preserve_3d { "preserve-3d".into() } else { "flat".into() },
        "backface-visibility" => if s.backface_hidden { "hidden".into() } else { "visible".into() },
        _ => crate::css::style::passthru_value(&s, prop).unwrap_or_default(),
    }
}

/// The physical content size sticky insets resolve percentages against: the
/// nearest scroll container's, else the viewport's.
fn sticky_basis(tree: &crate::layout::LayoutTree, b: usize) -> (f32, f32) {
    use crate::css::style::Overflow;
    let mut a = tree.boxes[b].parent;
    while let Some(i) = a {
        let st = &tree.boxes[i].style;
        let scrolls = |o: Overflow| !matches!(o, Overflow::Visible | Overflow::Clip);
        if tree.boxes[i].parent.is_some() && (scrolls(st.overflow_x) || scrolls(st.overflow_y)) {
            let (w, h) = (tree.boxes[i].content_w(), tree.boxes[i].content_h());
            return if frame_map(tree, i).is_some() { (h, w) } else { (w, h) };
        }
        a = tree.boxes[i].parent;
    }
    tree.viewport
}

/// The logical→physical side map of a box laid out in a rotated (vertical
/// writing-mode) frame, if it is in one.
fn frame_map(tree: &crate::layout::LayoutTree, b: usize) -> Option<[usize; 4]> {
    let bx = &tree.boxes[b];
    if matches!(bx.kind, crate::layout::BoxKind::Text(_) | crate::layout::BoxKind::Generated(_) | crate::layout::BoxKind::LineBreak) {
        return None;
    }
    let par = bx.parent?;
    if !(bx.style.is_vertical() && tree.boxes[par].style.is_vertical()) {
        return None;
    }
    Some(match bx.style.writing_mode {
        2 => [3, 2, 1, 0],
        4 => [3, 0, 1, 2],
        _ => [1, 2, 3, 0],
    })
}

/// A longhand's computed value from the property-value engine.
fn engine_computed(p: &mut crate::page::Page, s: &crate::css::style::ComputedStyle, prop: &str) -> Option<String> {
    use crate::css::pv;
    let env = engine_env(p, s);
    match engine_value(&env, s, prop) {
        Some(Some(v)) => Some(pv::hooks::ser_computed(prop, &v)),
        Some(None) => pv::initial_text(prop, &env),
        None => None,
    }
}

/// An engine property's computed value: None when held natively (animated
/// native fields), Some(None) when it can't be computed (use the initial value).
fn engine_value(env: &crate::css::pv::Env, s: &crate::css::style::ComputedStyle, prop: &str) -> Option<Option<crate::css::pv::V>> {
    use crate::css::pv;
    use crate::css::style::GenVal;
    let found = s.generic.as_ref().and_then(|g| crate::css::style::generic_find(g, prop));
    match found {
        Some(GenVal::Native) => None,
        Some(v @ (GenVal::Interp(_) | GenVal::Comp(_))) => crate::css::style::gen_eval(prop, &v, env).map(Some),
        Some(GenVal::Decl(v, em, lh)) if !v.is_empty() => {
            let mut env = env.clone();
            env.em = em as f64;
            env.ex = em as f64 * 0.5;
            env.ch = em as f64 * 0.5;
            env.lh = lh as f64;
            Some(pv::db::computed_value(prop, &v, &env).or_else(|| pv::db::computed_value(prop, &crate::css::tokenizer::parse_cvs(pv::db::initial_specified(prop)?), &env)))
        }
        _ => Some(pv::db::initial_specified(prop).and_then(|i| pv::db::computed_value(prop, &crate::css::tokenizer::parse_cvs(i), env))),
    }
}

/// `transform-origin` / `perspective-origin` resolved against the element's
/// border box (percentages become pixels).
fn resolved_origin(p: &mut crate::page::Page, id: NodeId, s: &crate::css::style::ComputedStyle, prop: &str) -> Option<String> {
    use crate::css::pv::{self, V};
    p.ensure_layout();
    let (w, h) = {
        let tree = p.layout.as_ref()?;
        let b = *tree.node_box.get(&id)?;
        let bx = &tree.boxes[b];
        if matches!(bx.kind, crate::layout::BoxKind::InlineBox) {
            return None;
        }
        (bx.w as f64, bx.h as f64)
    };
    let env = engine_env(p, s);
    let v = engine_value(&env, s, prop)??;
    let resolve = |x: &V, basis: f64| -> V {
        match x {
            V::Dim(n, u) if u == "%" => V::Dim(n * basis / 100.0, "px".into()),
            V::Math(m) => {
                let n = crate::css::calc::map_dims(&m.node, &|v, u| (u == "%").then(|| crate::css::calc::Node::Dim(v * basis / 100.0, "px".into())));
                match crate::css::calc::simplify(&n, &|_| None) {
                    crate::css::calc::Node::Dim(v, u) => V::Dim(v, u),
                    o => V::Math(Box::new(pv::MathV { node: o, kind: m.kind, range: m.range })),
                }
            }
            o => o.clone(),
        }
    };
    let out = match v {
        V::Seq(items) => V::Seq(items.iter().enumerate().map(|(i, x)| resolve(x, if i == 0 { w } else { h })).collect()),
        V::Pos(pp) => V::Seq(vec![resolve(pp.h.off.as_ref()?, w), resolve(pp.v.off.as_ref()?, h)]),
        o => o,
    };
    Some(pv::hooks::ser_computed(prop, &out))
}

/// The environment computed values resolve against for an element's style.
fn engine_env(p: &mut crate::page::Page, s: &crate::css::style::ComputedStyle) -> crate::css::pv::Env {
    use crate::css::pv;
    pv::URL_RESOLVE.get_or_init(|| |base, u| url::Url::parse(base).ok()?.join(u).ok().map(|x| x.to_string()));
    let (vw, vh) = p.viewport();
    let rem = p.doc.document_element().and_then(|r| p.styles.as_ref().and_then(|st| st.get(r)).map(|rs| rs.font_size)).unwrap_or(16.0) as f64;
    let current = match s.color_hp.as_ref().and_then(|h| h.iter().find(|(n, _)| *n == "color")) {
        Some((_, sp)) => {
            let legacy = crate::css::color::Abs::srgb(s.color.r as f64 / 255.0, s.color.g as f64 / 255.0, s.color.b as f64 / 255.0, s.color.a as f64 / 255.0);
            crate::css::color::compute(sp, &legacy, s.dark_scheme).unwrap_or(legacy)
        }
        None => crate::css::color::Abs::srgb(s.color.r as f64 / 255.0, s.color.g as f64 / 255.0, s.color.b as f64 / 255.0, s.color.a as f64 / 255.0),
    };
    let lh_px = match s.line_height {
        crate::css::style::LineHeight::Normal => s.font_size * 1.2,
        crate::css::style::LineHeight::Number(n) => s.font_size * n,
        crate::css::style::LineHeight::Px(v) => v,
    } as f64;
    pv::Env {
        em: s.font_size as f64,
        rem,
        ex: s.font_size as f64 * 0.5,
        ch: s.font_size as f64 * 0.5,
        lh: lh_px,
        rlh: rem * 1.2,
        vw: vw as f64,
        vh: vh as f64,
        cqw: vw as f64,
        cqh: vh as f64,
        current,
        dark: s.dark_scheme,
        base: p.doc.url.to_string(),
        zoom: 1.0,
    }
}

fn time_css(ms: f64) -> String {
    format!("{}s", num6(ms / 1000.0))
}

fn timing_css(t: &crate::css::anim::Timing) -> String {
    use crate::css::anim::Timing;
    match t {
        Timing::Linear => "linear".into(),
        Timing::Bezier(a, b, c, d) => match (*a, *b, *c, *d) {
            (0.25, 0.1, 0.25, 1.0) => "ease".into(),
            (0.42, 0.0, 1.0, 1.0) => "ease-in".into(),
            (0.0, 0.0, 0.58, 1.0) => "ease-out".into(),
            (0.42, 0.0, 0.58, 1.0) => "ease-in-out".into(),
            _ => format!("cubic-bezier({}, {}, {}, {})", num6(*a), num6(*b), num6(*c), num6(*d)),
        },
        Timing::Steps(1, 1) => "steps(1, start)".into(),
        Timing::Steps(n, pos) => match pos {
            0 => format!("steps({n})"),
            1 => format!("steps({n}, start)"),
            2 => format!("steps({n}, jump-none)"),
            _ => format!("steps({n}, jump-both)"),
        },
        Timing::Points(p) => format!("linear({})", p.iter().map(|(i, o)| format!("{} {}%", num6(*o as f64), num6(*i as f64 * 100.0))).collect::<Vec<_>>().join(", ")),
    }
}

fn anim_css(s: &crate::css::style::ComputedStyle, prop: &str) -> String {
    use crate::css::anim::{AnimSpec, Direction, Fill};
    let def = AnimSpec::default();
    let a: &AnimSpec = s.animation.as_deref().unwrap_or(&def);
    let names: Vec<String> = if a.names.is_empty() { vec!["none".into()] } else { a.names.iter().map(|n| n.to_string()).collect() };
    let dir = |d: &Direction| match d {
        Direction::Normal => "normal",
        Direction::Reverse => "reverse",
        Direction::Alternate => "alternate",
        Direction::AlternateReverse => "alternate-reverse",
    };
    let fill = |f: &Fill| match f {
        Fill::None => "none",
        Fill::Forwards => "forwards",
        Fill::Backwards => "backwards",
        Fill::Both => "both",
    };
    let iter = |n: &f64| if n.is_infinite() { "infinite".to_string() } else { num6(*n) };
    let join = |v: Vec<String>| v.join(", ");
    match prop {
        "animation-name" => join(names),
        "animation-duration" => join(a.durations.iter().map(|d| time_css(*d)).collect()),
        "animation-delay" => join(a.delays.iter().map(|d| time_css(*d)).collect()),
        "animation-timing-function" => join(a.timings.iter().map(timing_css).collect()),
        "animation-iteration-count" => join(a.iterations.iter().map(iter).collect()),
        "animation-direction" => join(a.directions.iter().map(|d| dir(d).to_string()).collect()),
        "animation-fill-mode" => join(a.fills.iter().map(|f| fill(f).to_string()).collect()),
        "animation-play-state" => join(a.paused.iter().map(|p| if *p { "paused".to_string() } else { "running".to_string() }).collect()),
        _ => join(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let (t, paused) = a.get(i);
                    let rest = format!("{} {} {} {} {} {} {}", time_css(t.duration), timing_css(&t.timing), time_css(t.delay), iter(&t.iterations), dir(&t.direction), fill(&t.fill), if paused { "paused" } else { "running" });
                    if n == "none" { format!("none {rest}") } else { format!("{rest} {n}") }
                })
                .collect(),
        ),
    }
}

fn trans_css(s: &crate::css::style::ComputedStyle, prop: &str) -> String {
    use crate::css::anim::TransSpec;
    let def = TransSpec::default();
    let t: &TransSpec = s.transition.as_deref().unwrap_or(&def);
    let props: Vec<String> = if t.props.is_empty() { vec!["none".into()] } else { t.props.iter().map(|p| p.to_string()).collect() };
    let join = |v: Vec<String>| v.join(", ");
    match prop {
        "transition-property" => join(props),
        "transition-duration" => join(t.durations.iter().map(|d| time_css(*d)).collect()),
        "transition-delay" => join(t.delays.iter().map(|d| time_css(*d)).collect()),
        "transition-timing-function" => join(t.timings.iter().map(timing_css).collect()),
        "transition-behavior" => join(t.allow_discrete.iter().map(|d| if *d { "allow-discrete".to_string() } else { "normal".to_string() }).collect()),
        _ => join(
            props
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    // Like Blink: defaults are left out.
                    let c = |n: usize| if n == 0 { 0 } else { i % n };
                    let dur = t.durations.get(c(t.durations.len())).copied().unwrap_or(0.0);
                    let delay = t.delays.get(c(t.delays.len())).copied().unwrap_or(0.0);
                    let tf = t.timings.get(c(t.timings.len())).cloned().unwrap_or(crate::css::anim::Timing::EASE);
                    let mut parts: Vec<String> = Vec::new();
                    if p != "all" {
                        parts.push(p.clone());
                    }
                    if dur != 0.0 || delay != 0.0 {
                        parts.push(time_css(dur));
                    }
                    if tf != crate::css::anim::Timing::EASE {
                        parts.push(timing_css(&tf));
                    }
                    if delay != 0.0 {
                        parts.push(time_css(delay));
                    }
                    if t.allow_discrete.get(c(t.allow_discrete.len())).copied().unwrap_or(false) {
                        parts.push("allow-discrete".into());
                    }
                    if parts.is_empty() { "all".to_string() } else { parts.join(" ") }
                })
                .collect(),
        ),
    }
}

/// A number the way Blink serializes computed values: up to 6 significant digits.
fn num6(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let mag = v.abs().log10().floor() as i32;
    let decimals = (5 - mag).clamp(0, 12) as usize;
    let s = format!("{:.*}", decimals, v);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// Serialize a box's four sides the way CSSOM shortens them (`10px`, `10px 20px`, ...).
fn four(v: &[String; 4]) -> String {
    if v[0] == v[1] && v[1] == v[2] && v[2] == v[3] {
        v[0].clone()
    } else if v[0] == v[2] && v[1] == v[3] {
        format!("{} {}", v[0], v[1])
    } else if v[1] == v[3] {
        format!("{} {} {}", v[0], v[1], v[2])
    } else {
        v.join(" ")
    }
}

/// Index (top, right, bottom, left) of the side named in a longhand property.
fn side(prop: &str) -> usize {
    if prop.contains("-top") {
        0
    } else if prop.contains("-right") {
        1
    } else if prop.contains("-bottom") {
        2
    } else {
        3
    }
}

fn border_style_css(b: crate::css::style::BorderStyle) -> &'static str {
    use crate::css::style::BorderStyle::*;
    match b {
        None => "none",
        Hidden => "hidden",
        Solid => "solid",
        Dashed => "dashed",
        Dotted => "dotted",
        Double => "double",
        Groove => "groove",
        Ridge => "ridge",
        Inset => "inset",
        Outset => "outset",
    }
}

fn radius_css(r: &(crate::css::values::Len, crate::css::values::Len)) -> String {
    let (a, b) = (r.0.to_css(), r.1.to_css());
    if a == b { a } else { format!("{a} {b}") }
}

fn gap_css(l: &crate::css::values::Len) -> String {
    if l.is_auto() { "normal".into() } else { l.to_css() }
}

fn svg_paint_css(p: &crate::css::style::SvgPaint, color: crate::css::values::Color) -> String {
    match p {
        crate::css::style::SvgPaint::Color(c) => c.resolve(color).to_css(),
        other => other.to_attr(color),
    }
}

/// The keyword for an alignment value; `flex` picks flex-start/flex-end spellings.
fn align_css(a: crate::css::style::Align, flex: bool) -> String {
    use crate::css::style::Align::*;
    match a {
        Auto => "auto",
        Normal => "normal",
        Stretch => "stretch",
        Start => if flex { "flex-start" } else { "start" },
        End => if flex { "flex-end" } else { "end" },
        Center => "center",
        Baseline => "baseline",
        SpaceBetween => "space-between",
        SpaceAround => "space-around",
        SpaceEvenly => "space-evenly",
        Left => "left",
        Right => "right",
    }
    .into()
}

fn inner_text(p: &mut crate::page::Page, id: NodeId) -> String {
    // HTML "innerText getter steps": an element that isn't being rendered returns its
    // text content; otherwise the rendered text collection steps run over the
    // children, with CSS white-space processing (collapsing, and removal at line
    // starts/ends) over each inline formatting context.
    use crate::css::style::{Display, Visibility};
    p.ensure_style();
    let styles = match &p.styles {
        Some(s) => s,
        None => return p.doc.text_content(id),
    };
    let doc = &p.doc;
    let in_select = |n: NodeId| -> bool {
        let mut c = doc.parent_element(n);
        while let Some(e) = c {
            if doc.is_tag(e, "select") {
                return true;
            }
            if !(doc.is_tag(e, "optgroup") || doc.is_tag(e, "option")) {
                return false;
            }
            c = doc.parent_element(e);
        }
        false
    };
    let rendered = |n: NodeId| -> bool {
        if !doc.is_connected(n) {
            return false;
        }
        let mut cur = Some(n);
        while let Some(c) = cur {
            if doc.element(c).is_some() {
                let opt = (doc.is_tag(c, "option") || doc.is_tag(c, "optgroup")) && in_select(c);
                match styles.get(c) {
                    Some(s) if s.display != Display::None || opt => {}
                    _ => return false,
                }
            }
            cur = doc.parent_element(c);
        }
        true
    };
    if !rendered(id) {
        return doc.text_content(id);
    }
    #[derive(Clone, Debug)]
    enum A {
        /// A collapsible space.
        Sp,
        /// Text that doesn't collapse.
        Txt(String),
        /// A forced line break (<br>, preserved segment break).
        Forced,
        /// An atomic inline (replaced element, inline-block…): content
        /// that holds no text of its own here.
        Obj,
        /// A required line break count (0: only a line boundary).
        Req(u8),
    }
    struct W<'a> {
        doc: &'a crate::dom::Document,
        styles: &'a crate::css::Styles,
    }
    const REPLACED: &[&str] = &["img", "input", "textarea", "iframe", "frame", "video", "audio", "canvas", "object", "embed", "meter", "progress", "frameset"];
    const SVG_HIDDEN: &[&str] = &["defs", "symbol", "clipPath", "mask", "pattern", "marker", "linearGradient", "radialGradient", "filter", "metadata", "title", "desc", "style", "script", "stop", "clippath", "lineargradient", "radialgradient"];
    impl<'a> W<'a> {
        fn table_ws_parent(d: Display) -> bool {
            matches!(d, Display::Table | Display::InlineTable | Display::TableRow | Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup | Display::TableColumn | Display::TableColumnGroup)
        }
        fn lang_tr(&self, n: NodeId) -> bool {
            let mut c = Some(n);
            while let Some(e) = c {
                if let Some(l) = self.doc.attr(e, "lang") {
                    let l = l.to_ascii_lowercase();
                    return l == "tr" || l.starts_with("tr-") || l == "az" || l.starts_with("az-");
                }
                c = self.doc.parent_element(e);
            }
            false
        }
        fn text(&self, n: NodeId, t: &str, out: &mut Vec<A>) {
            let doc = self.doc;
            let Some(parent) = doc.parent_element(n) else { return };
            let Some(ps) = self.styles.get(parent) else { return };
            if ps.visibility != Visibility::Visible {
                return;
            }
            // Whitespace-only text in table structure isn't rendered.
            if Self::table_ws_parent(ps.display) && t.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')) {
                return;
            }
            if let Some(pe) = doc.element(parent) {
                if pe.ns == crate::dom::Namespace::Svg && !matches!(pe.name.as_str(), "text" | "tspan" | "textPath" | "textpath" | "a") {
                    return;
                }
            }
            let collapse = ps.white_space.collapses_spaces();
            let keep_nl = ps.white_space.preserves_newlines();
            let tt = ps.text_transform;
            let tr = matches!(tt, crate::css::style::TextTransform::Uppercase) && self.lang_tr(parent);
            let mut buf = String::new();
            let flush = |buf: &mut String, out: &mut Vec<A>| {
                if !buf.is_empty() {
                    let s = if tr { buf.replace('i', "\u{130}") } else { std::mem::take(buf) };
                    buf.clear();
                    out.push(A::Txt(crate::layout::inline::transform_text(&s, tt, None)));
                }
            };
            let mut prev_cr = false;
            for c in t.chars() {
                let was_cr = prev_cr;
                prev_cr = c == '\r';
                match c {
                    '\n' | '\r' if keep_nl => {
                        if c == '\n' && was_cr {
                            continue;
                        }
                        flush(&mut buf, out);
                        out.push(A::Forced);
                    }
                    '\n' | '\r' | ' ' | '\t' if collapse => {
                        if c == '\n' && was_cr {
                            continue;
                        }
                        flush(&mut buf, out);
                        out.push(A::Sp);
                    }
                    '\r' => buf.push('\n'),
                    _ => buf.push(c),
                }
            }
            flush(&mut buf, out);
        }
        fn children(&self, n: NodeId, out: &mut Vec<A>) {
            for &c in self.doc.children(n) {
                self.node(c, out);
            }
        }
        /// Whether no later table-row (in the nearest ancestor table, document order) follows.
        fn last_row(&self, n: NodeId) -> bool {
            let doc = self.doc;
            let mut t = doc.parent_element(n);
            while let Some(e) = t {
                if self.styles.get(e).map(|s| matches!(s.display, Display::Table | Display::InlineTable)).unwrap_or(false) {
                    break;
                }
                t = doc.parent_element(e);
            }
            let Some(table) = t else { return true };
            let mut after = false;
            for d in doc.descendants(table) {
                if d == n {
                    after = true;
                    continue;
                }
                if after && doc.element(d).is_some() && self.styles.get(d).map(|s| s.display == Display::TableRow).unwrap_or(false) {
                    return false;
                }
            }
            true
        }
        fn last_cell(&self, n: NodeId) -> bool {
            let doc = self.doc;
            let Some(parent) = doc.parent(n) else { return true };
            let kids = doc.children(parent);
            let pos = kids.iter().position(|&k| k == n).unwrap_or(0);
            !kids[pos + 1..].iter().any(|&k| self.styles.get(k).map(|s| s.display == Display::TableCell).unwrap_or(false))
        }
        fn node(&self, n: NodeId, out: &mut Vec<A>) {
            let doc = self.doc;
            match &doc.node(n).data {
                NodeData::Text(t) => self.text(n, t, out),
                NodeData::Element(e) => self.element(n, e, out, false),
                _ => {}
            }
        }
        fn element(&self, n: NodeId, e: &crate::dom::ElementData, out: &mut Vec<A>, forced_block: bool) {
            let doc = self.doc;
            let Some(s) = self.styles.get(n) else { return };
            let name = e.name.as_str();
            let html = e.ns == crate::dom::Namespace::Html;
            if s.display == Display::None && !forced_block {
                return;
            }
            if e.ns == crate::dom::Namespace::Svg && SVG_HIDDEN.contains(&name) {
                return;
            }
            if html && name == "noscript" {
                return;
            }
            let visible = s.visibility == Visibility::Visible;
            // select: an inline box of its option/optgroup children (block-level).
            if html && name == "select" {
                for &c in doc.children(n) {
                    if let Some(ce) = doc.element(c) {
                        if ce.ns == crate::dom::Namespace::Html && (ce.name == "option" || ce.name == "optgroup") {
                            self.element(c, ce, out, true);
                        }
                    }
                }
                return;
            }
            if html && name == "optgroup" && forced_block {
                let start = out.len();
                for &c in doc.children(n) {
                    if let Some(ce) = doc.element(c) {
                        if ce.ns == crate::dom::Namespace::Html && ce.name == "option" {
                            self.element(c, ce, out, true);
                        }
                    }
                }
                if visible {
                    out.insert(start, A::Req(1));
                    out.push(A::Req(1));
                }
                return;
            }
            let replaced = (html && REPLACED.contains(&name)) || (e.ns == crate::dom::Namespace::Svg && name == "svg" && doc.parent_element(n).map(|p| doc.element(p).map(|pe| pe.ns != crate::dom::Namespace::Svg).unwrap_or(true)).unwrap_or(true));
            let atomic = matches!(s.display, Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable);
            if replaced && !(e.ns == crate::dom::Namespace::Svg) {
                if visible && s.display.is_inline_level() {
                    out.push(A::Obj);
                } else if visible {
                    out.push(A::Req(1));
                    out.push(A::Req(1));
                }
                return;
            }
            if html && name == "br" {
                if visible {
                    out.push(A::Forced);
                }
                return;
            }
            let start = out.len();
            if (atomic || replaced) && visible {
                out.push(A::Obj);
                out.push(A::Req(0));
            }
            let cell = s.display == Display::TableCell;
            if cell {
                out.push(A::Req(0));
            }
            if html && name == "details" && !e.has_attr("open") {
                if let Some(sum) = doc.element_children(n).find(|&c| doc.is_tag(c, "summary")) {
                    self.node(sum, out);
                }
            } else {
                self.children(n, out);
            }
            if !visible {
                return;
            }
            if cell {
                out.push(A::Req(0));
            }
            if atomic || replaced {
                out.push(A::Req(0));
                out.push(A::Obj);
            }
            if cell && !self.last_cell(n) {
                out.push(A::Txt("\t".into()));
            }
            if s.display == Display::TableRow && !self.last_row(n) {
                out.push(A::Txt("\n".into()));
            }
            let count = if html && name == "p" {
                2
            } else if forced_block || (!s.display.is_inline_level() && !matches!(s.display, Display::Contents | Display::TableCell | Display::TableRow | Display::TableColumn | Display::TableColumnGroup | Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup)) {
                1
            } else {
                0
            };
            if count > 0 {
                out.insert(start, A::Req(count));
                out.push(A::Req(count));
            }
        }
    }
    let w = W { doc, styles };
    let mut atoms = Vec::new();
    atoms.push(A::Req(0));
    match doc.element(id) {
        // A replaced element's children aren't rendered.
        Some(e) if e.ns == crate::dom::Namespace::Html && REPLACED.contains(&e.name.as_str()) => return String::new(),
        // A select renders its options (as blocks).
        Some(e) if e.ns == crate::dom::Namespace::Html && e.name == "select" => w.element(id, e, &mut atoms, false),
        _ => w.children(id, &mut atoms),
    }
    atoms.push(A::Req(0));
    // White-space processing: collapsible spaces collapse with the previous
    // one and vanish at line starts and ends (forced breaks, block boundaries).
    let mut res: Vec<A> = Vec::with_capacity(atoms.len());
    for a in atoms {
        match a {
            A::Sp => {
                match res.last() {
                    None | Some(A::Sp) | Some(A::Forced) | Some(A::Req(_)) => {}
                    _ => res.push(A::Sp),
                }
            }
            A::Forced | A::Req(_) => {
                while matches!(res.last(), Some(A::Sp)) {
                    res.pop();
                }
                res.push(a);
            }
            other => res.push(other),
        }
    }
    while matches!(res.last(), Some(A::Sp)) {
        res.pop();
    }
    // Required line breaks: runs become the largest count, and none at the
    // start or end.
    let mut out = String::new();
    let mut pending: u8 = 0;
    for a in res {
        match a {
            A::Req(c) => {
                if !out.is_empty() {
                    pending = pending.max(c);
                }
            }
            A::Obj => {}
            other => {
                let t = match other {
                    A::Sp => " ".to_string(),
                    A::Forced => "\n".to_string(),
                    A::Txt(t) => t,
                    _ => String::new(),
                };
                if t.is_empty() {
                    continue;
                }
                for _ in 0..pending {
                    out.push('\n');
                }
                pending = 0;
                out.push_str(&t);
            }
        }
    }
    out
}

// ----------------------------------------------------------- window natives

native!(n_viewport, |a| {
    let v = with_page(|p| {
        let (w, h) = p.viewport();
        let (dw, dh) = p.layout.as_ref().map(|l| (l.doc_width, l.doc_height)).unwrap_or((w, h));
        [w as f64, h as f64, (p.dpr * p.zoom) as f64, p.scroll.0 as f64, p.scroll.1 as f64, dw as f64, dh as f64]
    })
    .unwrap_or([1024.0, 768.0, 1.0, 0.0, 0.0, 1024.0, 768.0]);
    Ok(jnums(&v))
});

native!(n_scroll_to, |a| {
    let x = num(a, 0);
    let y = num(a, 1);
    with_page(|p| {
        let nx = if x.is_finite() { x as f32 } else { p.scroll.0 };
        let ny = if y.is_finite() { y as f32 } else { p.scroll.1 };
        let (vw, vh) = p.viewport();
        let max_y = p.layout.as_ref().map(|l| (l.doc_height - vh).max(0.0)).unwrap_or(0.0);
        let max_x = p.layout.as_ref().map(|l| (l.doc_width - vw).max(0.0)).unwrap_or(0.0);
        let ns = (nx.clamp(0.0, max_x), ny.clamp(0.0, max_y));
        if ns != p.scroll {
            p.scroll = ns;
            p.note_scrolled(crate::dom::DOCUMENT);
        }
        p.emit(crate::page::PageEvent::ScrollTo(p.scroll.0, p.scroll.1));
    });
    Ok(HostValue::Undefined)
});

native!(n_url, |a| { Ok(jstr(&with_page(|p| p.doc.url.to_string()).unwrap_or_default())) });

native!(n_navigate, |a| {
    let u = st(a, 0);
    let replace = boolean(a, 1, false);
    with_page(|p| {
        if let Some(abs) = p.doc.resolve_url(&u) {
            if abs.scheme() == "javascript" {
                return;
            }
            if abs.scheme() == "file" && p.doc.url.scheme() != "file" {
                return;
            }
            // Same document, another fragment: navigate now (location reads
            // the new URL at once; hashchange is queued).
            let mut a = abs.clone();
            a.set_fragment(None);
            let mut b = p.doc.url.clone();
            b.set_fragment(None);
            if a == b && abs.fragment().is_some() && p.frame_parent.is_none() && p.layout.is_some() {
                p.fragment_navigate(abs, replace);
                return;
            }
            let mut r = crate::page::NavRequest::get(abs.as_str());
            r.push = !replace;
            r.referrer = Some(p.doc.url.clone());
            p.pending_nav = Some(r);
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_reload, |a| {
    with_page(|p| {
        let mut r = crate::page::NavRequest::get(p.doc.url.as_str());
        r.push = false;
        p.pending_nav = Some(r);
    });
    Ok(HostValue::Undefined)
});

native!(n_history_go, |a| {
    let d = num(a, 0);
    let d = if d.is_finite() { d as i32 } else { 0 };
    let tx = with_page(|p| p.tx.clone());
    if let Some(tx) = tx {
        if d < 0 {
            let _ = tx.send(PageMsg::Back);
        } else if d > 0 {
            let _ = tx.send(PageMsg::Forward);
        } else {
            let _ = tx.send(PageMsg::Reload);
        }
    }
    Ok(HostValue::Undefined)
});

native!(n_push_state, |a| {
    let u = opt_st(a, 0);
    let replace = boolean(a, 1, false);
    let state = st(a, 2);
    with_page(|p| {
        let url = u.and_then(|u| p.doc.resolve_url(&u));
        p.history_state_json = state;
        p.push_state(url, replace);
    });
    Ok(HostValue::Undefined)
});

native!(n_history_state, |a| { Ok(jstr(&with_page(|p| p.history_state_json.clone()).unwrap_or_else(|| "null".into()))) });

native!(n_title, |a| { Ok(jstr(&with_page(|p| p.doc.title()).unwrap_or_default())) });

native!(n_set_title, |a| {
    let t = st(a, 0);
    with_page(|p| {
        p.doc.title_override = Some(t.clone());
        p.emit(crate::page::PageEvent::Title(t));
    });
    Ok(HostValue::Undefined)
});

native!(n_cookie, |a| {
    let v = with_page(|p| {
        let u = p.doc.url.clone();
        crate::net::client().cookies.lock().unwrap().script_cookies(&u)
    })
    .unwrap_or_default();
    Ok(jstr(&v))
});

native!(n_set_cookie, |a| {
    let s = st(a, 0);
    with_page(|p| {
        let u = p.doc.url.clone();
        if matches!(u.scheme(), "http" | "https") {
            crate::net::client().cookies.lock().unwrap().set_from_script(&s, &u);
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_storage, |a| {
    // (op, kind, key, value): op 0 get, 1 set, 2 remove, 3 clear, 4 keys, 5 length.
    // localStorage lives in the profile store (shared by tabs, saved to disk);
    // sessionStorage stays with the page. A set over quota returns false.
    let op = num(a, 0) as i32;
    let session = boolean(a, 1, false);
    let key = st(a, 2);
    let val = st(a, 3);
    let origin = with_page(|p| p.doc.url.origin().ascii_serialization()).unwrap_or_default();
    if !session {
        use crate::storage as s;
        return Ok(match op {
            0 => s::local_get(&origin, &key).map(|v| jstr(&v)).unwrap_or(HostValue::Null),
            1 => HostValue::Bool(s::local_set(&origin, &key, &val)),
            2 => {
                s::local_remove(&origin, &key);
                HostValue::Null
            }
            3 => {
                s::local_clear(&origin);
                HostValue::Null
            }
            4 => jstr(&s::local_keys(&origin).join("\u{0}")),
            _ => HostValue::Null,
        });
    }
    let r = with_page(|p| {
        let map = &mut p.session_storage;
        let k = format!("{origin}\u{0}{key}");
        match op {
            0 => map.get(&k).cloned().map(Some),
            1 => {
                if val.len() + k.len() < 5_000_000 {
                    map.insert(k, val);
                    None
                } else {
                    Some(None)
                }
            }
            2 => {
                map.remove(&k);
                None
            }
            3 => {
                let prefix = format!("{origin}\u{0}");
                map.retain(|kk, _| !kk.starts_with(&prefix));
                None
            }
            4 => {
                let prefix = format!("{origin}\u{0}");
                let keys: Vec<String> = map.keys().filter(|kk| kk.starts_with(&prefix)).map(|kk| kk[prefix.len()..].to_string()).collect();
                Some(Some(keys.join("\u{0}")))
            }
            _ => None,
        }
    })
    .flatten();
    Ok(match (op, r) {
        (1, Some(None)) => HostValue::Bool(false),
        (1, _) => HostValue::Bool(true),
        (_, Some(Some(s))) => jstr(&s),
        _ => HostValue::Null,
    })
});

fn page_origin() -> String {
    with_page(|p| p.doc.url.origin().ascii_serialization()).unwrap_or_default()
}

native!(n_idb_load, |a| {
    // (name) -> saved snapshot string or null (only with a persistent profile).
    if !crate::storage::is_persistent() {
        return Ok(HostValue::Null);
    }
    Ok(crate::storage::idb_get(&page_origin(), &st(a, 0)).map(|s| HostValue::String(s)).unwrap_or(HostValue::Null))
});

native!(n_idb_save, |a| {
    if crate::storage::is_persistent() {
        let origin = page_origin();
        if origin != "null" {
            crate::storage::idb_put(&origin, &st(a, 0), &st(a, 1));
        }
    }
    Ok(HostValue::Undefined)
});

native!(n_idb_delete, |a| {
    if crate::storage::is_persistent() {
        crate::storage::idb_delete(&page_origin(), &st(a, 0));
    }
    Ok(HostValue::Undefined)
});

native!(n_idb_names, |_a| {
    if !crate::storage::is_persistent() {
        return Ok(HostValue::Array(Vec::new()));
    }
    Ok(HostValue::Array(crate::storage::idb_names(&page_origin()).into_iter().map(HostValue::String).collect()))
});

native!(n_permission_request, |a| {
    // (permission, id): the browser answers through __ohb_permReply(id, ok, data).
    let perm = st(a, 0);
    let id = num(a, 1) as u64;
    let site = page_origin();
    with_page(|p| {
        let opaque = !(site.starts_with("https://") || site.starts_with("http://"));
        if p.frame_parent.is_some() || opaque || std::env::var_os("OHB_WPT").is_some() && perm != "clipboard-read" {
            p.js_tasks.push(crate::page::JsTask::CallGlobal("__ohb_permReply", vec![HostValue::Number(id as f64), HostValue::Bool(false), HostValue::Null]));
        } else {
            p.emit(crate::page::PageEvent::Permission { id, perm, site });
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_permission_state, |a| {
    let perm = st(a, 0);
    let site = page_origin();
    Ok(HostValue::String(
        match crate::prefs::permission(&site, &perm) {
            crate::prefs::Decision::Allow => "granted",
            crate::prefs::Decision::Block => "denied",
            crate::prefs::Decision::Ask => "prompt",
        }
        .into(),
    ))
});

native!(n_clipboard_write, |a| {
    let s = st(a, 0);
    with_page(|p| {
        if p.frame_parent.is_none() {
            p.emit(crate::page::PageEvent::Copy(s));
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_paste_text, |a| {
    let s = st(a, 0);
    with_page(|p| p.on_text(s));
    Ok(HostValue::Undefined)
});

native!(n_alert, |a| {
    let s = st(a, 0);
    with_page(|p| p.emit(crate::page::PageEvent::Alert(s)));
    Ok(HostValue::Undefined)
});

native!(n_log, |a| {
    let level = st(a, 0);
    let msg = st(a, 1);
    let quiet = boolean(a, 3, false);
    if std::env::var("OHB_CONSOLE").is_ok() || level == "error" && !quiet && std::env::var("OHB_QUIET").is_err() {
        let frame = with_page(|p| p.frame_parent.as_ref().map(|_| format!(" (frame {})", p.doc.url.path()))).flatten().unwrap_or_default();
        eprintln!("[console.{level}]{frame} {}", msg.chars().take(2000).collect::<String>());
    }
    let args = if a.len() > 2 { st(a, 2) } else { String::new() };
    with_page(|p| p.console_add(&level, msg, args));
    Ok(HostValue::Undefined)
});

// ---- Live frames: windows on other threads, reached by postMessage.
native!(n_frame_remote, |a| {
    let n = num(a, 0) as NodeId;
    Ok(HostValue::Number(with_page(|p| p.live_frame_id(n)).flatten().map(|i| i as f64).unwrap_or(-1.0)))
});

// Would an iframe with this (resolved) src run live here?
native!(n_frame_live_ok, |a| {
    // (url, iframe?): does a frame showing url run live (on its own thread)?
    let u = st(a, 0);
    let f = nid(a, 1);
    Ok(HostValue::Bool(with_page(|p| {
        let local = match f {
            Some(f) => p.local_frame_ok(f, &u),
            None => p.js_enabled && p.local_url_ok(&u),
        };
        !local && p.js_enabled && crate::page::frames::live_scheme(&u) && p.frame_parent.as_ref().map(|f| f.depth < 3).unwrap_or(true) && std::env::var_os("OHB_STATIC_FRAMES").is_none()
    }).unwrap_or(false)))
});

native!(n_frame_local_ok, |a| {
    // (iframe, url): is a frame showing url local (same origin, on this thread)?
    let f = nid(a, 0);
    let u = st(a, 1);
    Ok(HostValue::Bool(f.and_then(|f| with_page(|p| p.local_frame_ok(f, &u))).unwrap_or(false)))
});

native!(n_frame_doc, |a| {
    // (iframe, document, url): the local frame now shows this document.
    let f = nid(a, 0);
    let d = nid(a, 1);
    let url = st(a, 2);
    if let (Some(f), Some(d)) = (f, d) {
        with_page(|p| {
            p.doc.frame_of.insert(d, f);
            p.local_frame_doc(f, d, url);
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_frames_pending, |a| {
    // (document) -> whether frames in it are still loading.
    Ok(HostValue::Bool(nid(a, 0).and_then(|d| with_page(|p| p.frames_pending_in(d))).unwrap_or(false)))
});

native!(n_frame_loaded, |a| {
    if let Some(f) = nid(a, 0) {
        with_page(|p| p.local_frame_loaded(f));
    }
    Ok(HostValue::Undefined)
});

native!(n_frame_navigate, |a| {
    // (iframe, absolute url, referrer?) -> whether the frame navigates.
    let f = nid(a, 0);
    let u = st(a, 1);
    let referrer = opt_st(a, 2).and_then(|r| url::Url::parse(&r).ok());
    Ok(HostValue::Bool(f.and_then(|f| with_page(|p| p.local_frame_navigate(f, &u, referrer))).unwrap_or(false)))
});

fn pid_arg(a: &mut dyn HostArgs, i: usize) -> Vec<u64> {
    match a.value(i) {
        Ok(HostValue::Array(v)) => v.into_iter().filter_map(|x| if let HostValue::Number(n) = x { Some(n as u64) } else { None }).collect(),
        _ => Vec::new(),
    }
}

native!(n_frame_post, |a| {
    let n = num(a, 0) as NodeId;
    let data = a.value(1)?;
    let target_origin = st(a, 2);
    let ports = pid_arg(a, 3);
    with_page(|p| {
        let origin = p.doc.url.origin().ascii_serialization();
        if std::env::var_os("OHB_DEBUG_FRAMES").is_some() {
            eprintln!("[frames] {origin} posts to frame node {n} (live={:?}) target={target_origin} ports={ports:?}: {}", p.live_frame_id(n), format!("{data:?}").chars().take(300).collect::<String>());
        }
        if let Some(fid) = p.live_frame_id(n) {
            crate::page::frames::port_transfer(&ports, fid);
        }
        p.send_to_frame(n, crate::page::PageMsg::PostMessage { data, origin, target_origin, ports, source: None });
    });
    Ok(HostValue::Undefined)
});

// (data, targetOrigin, to, ports): `to` is "parent", "top" or ">name" for a
// sibling frame (parent.frames[name]).
native!(n_parent_post, |a| {
    let data = a.value(0)?;
    let target_origin = st(a, 1);
    let to = st(a, 2);
    let ports = pid_arg(a, 3);
    let to = match to.as_str() {
        "top" => crate::page::frames::MsgTarget::Top,
        t if t.starts_with('>') => crate::page::frames::MsgTarget::Sibling(t[1..].to_string()),
        _ => crate::page::frames::MsgTarget::Parent,
    };
    with_page(|p| {
        if let Some(fp) = &p.frame_parent {
            let origin = p.doc.url.origin().ascii_serialization();
            crate::page::frames::port_transfer(&ports, fp.parent_id);
            let _ = fp.tx.send(PageMsg::Child(fp.frame_id, crate::page::frames::ChildMsg::Message { data, origin, target_origin, to, ports }));
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_port_pair, |a| {
    let owner = crate::page::frames::thread_owner();
    let (x, y) = crate::page::frames::port_pair(owner);
    Ok(HostValue::Array(vec![HostValue::Number(x as f64), HostValue::Number(y as f64)]))
});

native!(n_port_post, |a| {
    let pid = num(a, 0) as u64;
    let data = a.value(1)?;
    let transfer = pid_arg(a, 2);
    crate::page::frames::port_post(pid, data, transfer);
    Ok(HostValue::Undefined)
});

native!(n_port_close, |a| {
    crate::page::frames::port_close(num(a, 0) as u64);
    Ok(HostValue::Undefined)
});

// For a frame page: [parent origin, depth, parent URL]; null at top level.
native!(n_frame_info, |a| {
    Ok(with_page(|p| {
        p.frame_parent.as_ref().map(|f| HostValue::Array(vec![HostValue::String(f.parent_url.origin().ascii_serialization()), HostValue::Number(f.depth as f64), HostValue::String(f.parent_url.to_string()), HostValue::String(f.name.clone())]))
    })
    .flatten()
    .unwrap_or(HostValue::Null))
});

native!(n_navigate_top, |a| {
    let u = st(a, 0);
    with_page(|p| {
        if let (Some(abs), Some(fp)) = (p.doc.resolve_url(&u), p.frame_parent.as_ref()) {
            if matches!(abs.scheme(), "http" | "https") {
                let _ = fp.tx.send(PageMsg::Child(fp.frame_id, crate::page::frames::ChildMsg::NavigateTop(abs.to_string())));
            }
        }
    });
    Ok(HostValue::Undefined)
});

// ---- Test automation (web-platform-tests' testdriver): only with OHB_WPT set.
// Input goes through the same paths as real input, so events are trusted and
// default actions run.
pub fn wpt_enabled() -> bool {
    std::env::var_os("OHB_WPT").is_some()
}

native!(n_scroll_into_view, |a| {
    let id = nid(a, 0);
    let block = st(a, 1);
    let inline = st(a, 2);
    if let Some(id) = id {
        with_page(|p| p.scroll_into_view(id, &block, &inline));
    }
    Ok(HostValue::Undefined)
});

native!(n_link_sheet_text, |a| {
    // [text, sheetURL] of a loaded <link> sheet, or null.
    let id = nid(a, 0);
    let r = id.and_then(|id| with_page(|p| p.link_sheet_text(id)).flatten());
    Ok(match r {
        Some((t, u)) => HostValue::Array(vec![jstr(&t), jstr(u.as_str())]),
        None => HostValue::Null,
    })
});

native!(n_import_sheet_text, |a| {
    // The text of a loaded @import sheet by URL, or null.
    let u = st(a, 0);
    let r = with_page(|p| p.import_texts.get(&u).cloned()).flatten();
    Ok(match r {
        Some(t) => jstr(&t),
        None => HostValue::Null,
    })
});

native!(n_link_sheet_gen, |a| {
    let id = nid(a, 0);
    Ok(HostValue::Number(id.and_then(|id| with_page(|p| p.link_sheet_gen(id))).unwrap_or(-1.0)))
});

native!(n_set_link_sheet_text, |a| {
    let id = nid(a, 0);
    let t = st(a, 1);
    if let Some(id) = id {
        with_page(|p| p.set_link_sheet_text(id, &t));
    }
    Ok(HostValue::Undefined)
});

// ---- CSS Font Loading (document.fonts / FontFace)
native!(n_font_faces, |a| {
    // [key, family, wMin, wMax, italic, unicodeRange, status(0..3)] per declared @font-face.
    // The page's @font-face rules are collected when styles are computed.
    let list = with_page(|p| {
        if !p.in_frame_ctx() {
            p.flush_pending_sheets();
            p.refresh_rule_index();
        }
        p.declared_font_list()
    })
    .unwrap_or_default();
    Ok(HostValue::Array(
        list.into_iter()
            .map(|(k, f, w, it, ur, st)| HostValue::Array(vec![jstr(&k), jstr(&f), HostValue::from(w.0 as f64), HostValue::from(w.1 as f64), HostValue::from(it), jstr(&ur), HostValue::from(st as f64)]))
            .collect(),
    ))
});

native!(n_font_load, |a| {
    let fam = st(a, 0);
    let w = num(a, 1);
    let w = if w.is_finite() { w.clamp(1.0, 1000.0) as u16 } else { 400 };
    let italic = boolean(a, 2, false);
    let text = st(a, 3);
    let keys = with_page(|p| {
        if !p.in_frame_ctx() {
            p.flush_pending_sheets();
            p.refresh_rule_index();
        }
        p.load_font_family(&fam, w, italic, &text)
    })
    .unwrap_or_default();
    Ok(HostValue::Array(keys.iter().map(|k| jstr(k)).collect()))
});

// A FontFace added from script (document.fonts.add): the page then tells the
// script side which families its styles use, so those faces load on use.
native!(n_font_face_added, |a| {
    let fam = st(a, 0).to_ascii_lowercase();
    with_page(|p| {
        if p.js_font_families.insert(fam) {
            p.fonts_rescan = true;
            p.need_style = true;
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_fonts_pending, |a| {
    // Faces loading, plus "1" while a stylesheet is still loading (it may
    // declare or use faces). Styles are brought up to date first: that is
    // what starts the loads of the faces the page uses.
    Ok(HostValue::from(
        with_page(|p| {
            if !p.in_frame_ctx() {
                p.ensure_style();
            }
            p.font_status.values().filter(|v| **v == 1).count() + usize::from(p.loading && p.sheets_loading())
        })
        .unwrap_or(0) as f64,
    ))
});

native!(n_add_font_bytes, |a| {
    // (family, wMin, wMax, italic, bytes, unicodeRange) -> registered?
    let fam = st(a, 0);
    let w0 = num(a, 1);
    let w1 = num(a, 2);
    let italic = boolean(a, 3, false);
    let bytes = match a.value(4)? {
        HostValue::Bytes(b) => b,
        _ => Vec::new(),
    };
    let ranges = crate::css::parse_unicode_range(&st(a, 5));
    let w = |v: f64| if v.is_finite() { v.clamp(1.0, 1000.0) as u16 } else { 400 };
    let style = if italic { crate::css::style::FontStyle::Italic } else { crate::css::style::FontStyle::Normal };
    let ok = with_page(|p| {
        let ok = p.fonts.add_web_font(&fam, (w(w0), w(w1)), style, &bytes, ranges);
        if ok {
            p.need_layout = true;
            p.need_style = true;
        }
        ok
    })
    .unwrap_or(false);
    Ok(HostValue::from(ok))
});

native!(n_note_wheel, |a| {
    // (blocking): a wheel listener that may cancel scrolling, or a passive one.
    let blocking = a.value(0).map(|v| v.truthy()).unwrap_or(true);
    with_page(|p| {
        if blocking && !p.wheel_listeners {
            p.wheel_listeners = true;
            p.need_paint = true;
        } else if !blocking && !p.wheel_passive {
            p.wheel_passive = true;
            p.need_paint = true;
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_wpt_enabled, |a| { Ok(HostValue::Bool(wpt_enabled())) });

native!(n_wpt_input, |a| {
    if !wpt_enabled() {
        return Ok(HostValue::Undefined);
    }
    let kind = st(a, 0);
    let (x, y) = (num(a, 1) as f32, num(a, 2) as f32);
    let m = num(a, 5);
    let m = if m.is_finite() { m as u32 } else { 0 };
    let mods = crate::page::Mods { shift: m & 1 != 0, ctrl: m & 2 != 0, alt: m & 4 != 0, meta: m & 8 != 0 };
    let n3 = num(a, 3);
    let button = if n3.is_finite() { n3 as u8 } else { 0 };
    with_page(|p| {
        p.render();
        match kind.as_str() {
            "move" => {
                p.handle(PageMsg::MouseMove { x, y });
            }
            "down" => {
                p.handle(PageMsg::MouseDown { x, y, button, mods });
            }
            "up" => {
                p.handle(PageMsg::MouseUp { x, y, button, mods });
            }
            "wheel" => {
                p.handle(PageMsg::Wheel { x, y, dx: num(a, 3) as f32, dy: num(a, 4) as f32 });
            }
            // An automation action chain ended: so does its wheel transaction.
            "wheelend" => {
                p.wheel_tx = None;
            }
            "key" => {
                let name = st(a, 1);
                let text = st(a, 2);
                use crate::page::Key;
                let key = match name.as_str() {
                    "Backspace" => Key::Backspace,
                    "Delete" => Key::Delete,
                    "ArrowLeft" => Key::Left,
                    "ArrowRight" => Key::Right,
                    "ArrowUp" => Key::Up,
                    "ArrowDown" => Key::Down,
                    "Home" => Key::Home,
                    "End" => Key::End,
                    "Enter" => Key::Enter,
                    "Tab" => Key::Tab,
                    "Escape" => Key::Escape,
                    "PageUp" => Key::PageUp,
                    "PageDown" => Key::PageDown,
                    " " => Key::Space,
                    n if n.chars().count() == 1 => Key::Char(n.to_string()),
                    n => Key::Other(n.to_string()),
                };
                p.handle(PageMsg::Key { key, mods });
                if !text.is_empty() && !mods.ctrl && !mods.meta {
                    p.handle(PageMsg::Text(text));
                }
            }
            _ => {}
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_wpt_sync, |a| {
    if wpt_enabled() {
        let id = num(a, 0);
        with_page(|p| p.js_tasks.push(crate::page::JsTask::CallGlobal("__ohb_wptSync", vec![HostValue::Number(id)])));
    }
    Ok(HostValue::Undefined)
});

// ---- DevTools: the console asks whether to build rich previews; a
// DevTools page (only) sends requests to the tab it inspects.
native!(n_dt_attached, |a| {
    Ok(HostValue::Bool(with_page(|p| p.dt.client.is_some()).unwrap_or(false)))
});

native!(n_dt_send, |a| {
    let method = st(a, 0);
    let params = st(a, 1);
    let id = with_page(|p| {
        let target = p.dt.target?;
        p.dt.next_req += 1;
        let id = p.dt.next_req;
        let from = p.tab_id;
        if crate::page::devtools::send(target, crate::page::devtools::DevMsg::Request { from, id, method, params }) {
            Some(id)
        } else {
            None
        }
    })
    .flatten();
    Ok(id.map(|i| HostValue::Number(i as f64)).unwrap_or(HostValue::Number(-1.0)))
});

native!(n_dt_close, |a| {
    with_page(|p| p.emit(crate::page::PageEvent::DevtoolsClose));
    Ok(HostValue::Undefined)
});

native!(n_dt_target, |a| {
    Ok(with_page(|p| p.dt.target).flatten().map(|t| HostValue::Number(t as f64)).unwrap_or(HostValue::Null))
});

native!(n_now, |a| {
    Ok(HostValue::from(page_now_ms()))
});

native!(n_ease_eval, |a| {
    // (easing text, input progress, before flag): the transformed progress.
    let t = crate::css::anim::parse_timing(&crate::css::tokenizer::parse_cvs(&st(a, 0))).unwrap_or(crate::css::anim::Timing::Linear);
    Ok(HostValue::from(t.eval_flag(num(a, 1), a.value(2).map(|v| v.truthy()).unwrap_or(false))))
});

native!(n_css_animatable, |a| {
    // Whether a CSS property name can appear in keyframes (Web Animations).
    let n = st(a, 0);
    let c = crate::css::pv::db::canonical_name(&n);
    Ok(HostValue::from((crate::css::pv::is_known(&c) || crate::css::known_property(&c)) && crate::css::pv::interp::animatable(&c) && c != "all"))
});

native!(n_css_number, |a| {
    // A CSS <number> (or calc() of numbers; percentages as fractions); NaN if not one.
    let cvs = crate::css::tokenizer::parse_cvs(&st(a, 0));
    let it: Vec<&crate::css::tokenizer::CV> = cvs.iter().filter(|c| !c.is_ws()).collect();
    use crate::css::tokenizer::{Tok, CV};
    let v = match it.as_slice() {
        [CV::T(Tok::Number(n))] => *n,
        [CV::T(Tok::Percentage(p))] => *p / 100.0,
        [CV::Func(name, args)] if crate::css::calc::is_math_fn(name) => match crate::css::calc::parse_fn(name, args).map(|n| crate::css::calc::simplify(&n, &|_| None)) {
            Some(crate::css::calc::Node::Num(n)) => n,
            Some(crate::css::calc::Node::Dim(n, u)) if u == "%" => n / 100.0,
            _ => f64::NAN,
        },
        _ => f64::NAN,
    };
    Ok(HostValue::from(v))
});

native!(n_css_longhands, |a| {
    // The longhands of a (shorthand) property, or the property itself.
    let n = crate::css::pv::db::canonical_name(&st(a, 0));
    Ok(match crate::css::pv::shorthand::longhands(&n) {
        Some(l) => jstrs(l.into_iter().map(|x| x.to_string()).collect()),
        None => jstrs(vec![n]),
    })
});

native!(n_css_anims, |a| {
    // [[id, node, pseudo, transition?, name, start, pausedAt|NaN, delay, duration, iterations, direction, fill, easing]...]
    // (Pending style changes start or cancel transitions first.)
    let list = with_page(|p| {
        p.ensure_style();
        p.anim.css_list()
    })
    .unwrap_or_default();
    let rows: Vec<HostValue> = list
        .into_iter()
        .map(|i| {
            let pseudo = if i.node & crate::css::ANIM_BEFORE != 0 { "::before" } else if i.node & crate::css::ANIM_AFTER != 0 { "::after" } else { "" };
            let node = i.node & !(crate::css::ANIM_BEFORE | crate::css::ANIM_AFTER);
            HostValue::Array(vec![
                HostValue::from(i.id as f64),
                HostValue::from(node as f64),
                jstr(pseudo),
                HostValue::from(i.transition),
                jstr(&i.name),
                HostValue::from(i.start),
                HostValue::from(i.paused_at.unwrap_or(f64::NAN)),
                HostValue::from(i.delay),
                HostValue::from(i.duration),
                HostValue::from(i.iterations),
                jstr(i.direction),
                jstr(i.fill),
                jstr(&i.easing),
            ])
        })
        .collect();
    Ok(HostValue::Array(rows))
});

native!(n_css_anim_seek, |a| {
    let id = num(a, 0) as u64;
    let t = num(a, 1);
    with_page(|p| {
        let now = crate::js::host::timeline_ms();
        if p.anim.css_seek(id, t, now) {
            p.need_style = true;
            { p.full_style = true; crate::page::full_why(file!(), line!()); }
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_css_anim_play, |a| {
    let id = num(a, 0) as u64;
    let play = boolean(a, 1, true);
    with_page(|p| {
        p.anim.css_play(id, play, crate::js::host::timeline_ms());
        p.need_style = true;
        { p.full_style = true; crate::page::full_why(file!(), line!()); }
    });
    Ok(HostValue::Undefined)
});

native!(n_css_anim_cancel, |a| {
    let id = num(a, 0) as u64;
    with_page(|p| {
        p.anim.css_cancel(id);
        p.need_style = true;
        { p.full_style = true; crate::page::full_why(file!(), line!()); }
    });
    Ok(HostValue::Undefined)
});

native!(n_easing_canon, |a| {
    // The canonical serialization of one <easing-function>, or null if invalid.
    let t = st(a, 0);
    let cvs = crate::css::tokenizer::parse_cvs(&t);
    if crate::css::tokenizer::split_commas(&cvs).len() != 1 || crate::css::anim::parse_timing(&cvs).is_none() {
        return Ok(HostValue::Null);
    }
    Ok(match crate::css::pv::db::specified_text("animation-timing-function", &t) {
        Some(c) => HostValue::from(c),
        None => HostValue::Null,
    })
});

native!(n_timeline_now, |a| {
    Ok(HostValue::from(crate::js::host::timeline_ms()))
});

thread_local! {
    static RNG: std::cell::Cell<u64> = std::cell::Cell::new(seed());
}

fn seed() -> u64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    let addr = &t as *const u64 as u64;
    t ^ addr.rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15
}

fn next_rand() -> u64 {
    RNG.with(|r| {
        // xorshift64*
        let mut x = r.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        r.set(x);
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    })
}

native!(n_random_bytes, |a| {
    let n = num(a, 0);
    let n = if n.is_finite() { (n as usize).min(65536) } else { 16 };
    let v: Vec<f64> = (0..n).map(|_| (next_rand() & 0xFF) as f64).collect();
    Ok(jnums(&v))
});

native!(n_resolve_url, |a| {
    // (url, base?, encoding?): queries are encoded in a legacy document encoding.
    let u = st(a, 0);
    let base = opt_st(a, 1);
    let enc = opt_st(a, 2).filter(|e| !e.eq_ignore_ascii_case("utf-8"));
    let r = match base {
        Some(b) => url::Url::parse(&b).ok().and_then(|b| match enc.as_deref().and_then(crate::html::encoding::canonical_name) {
            Some(cs) if u.contains('?') => {
                let href = u.trim_matches(|c: char| c <= ' ');
                let encf = crate::html::encoding::override_fn(move |q: &str| {
                    std::borrow::Cow::Owned(crate::html::encoding::encode_with(cs, q, &|c, out: &mut Vec<u8>| out.extend_from_slice(format!("%26%23{}%3B", c as u32).as_bytes())))
                });
                url::Url::options().base_url(Some(&b)).encoding_override(Some(&encf)).parse(href).ok()
            }
            _ => b.join(&u).ok(),
        }),
        None => with_page(|p| p.doc.resolve_url(&u)).flatten(),
    };
    Ok(jopt(r.map(|x| x.to_string())))
});

native!(n_url_parts, |a| {
    let u = st(a, 0);
    let base = opt_st(a, 1);
    let parsed = match base {
        Some(b) => url::Url::parse(&b).ok().and_then(|b| b.join(&u).ok()),
        None => url::Url::parse(&u).ok(),
    };
    match parsed {
        None => Ok(HostValue::Null),
        Some(x) => {
            // The URL Standard's getters (url::quirks).
            use url::quirks as q;
            let origin = x.origin().ascii_serialization();
            let parts = vec![
                q::href(&x).to_string(),
                q::protocol(&x).to_string(),
                q::host(&x).to_string(),
                q::hostname(&x).to_string(),
                q::port(&x).to_string(),
                q::pathname(&x).to_string(),
                q::search(&x).to_string(),
                q::hash(&x).to_string(),
                if origin == "null" { "null".into() } else { origin },
                q::username(&x).to_string(),
                q::password(&x).to_string(),
            ];
            Ok(jstrs(parts))
        }
    }
});

native!(n_form_decode, |a| {
    // application/x-www-form-urlencoded parsing -> [name, value, ...]
    let s = st(a, 0);
    let mut out = Vec::new();
    for (k, v) in url::form_urlencoded::parse(s.as_bytes()) {
        out.push(k.into_owned());
        out.push(v.into_owned());
    }
    Ok(jstrs(out))
});

native!(n_form_encode, |a| {
    // [name, value, ...] -> application/x-www-form-urlencoded
    let v = match a.value(0)? {
        HostValue::Array(v) => v,
        _ => Vec::new(),
    };
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for pair in v.chunks(2) {
        if let [k, val] = pair {
            ser.append_pair(&k.string(), &val.string());
        }
    }
    Ok(jstr(&ser.finish()))
});

native!(n_base_url, |_a| { Ok(jstr(&with_page(|p| p.doc.current_base().to_string()).unwrap_or_default())) });

native!(n_url_set, |a| {
    // (href, part, value) -> the href after the URL Standard's setter (null if href doesn't parse).
    let href = st(a, 0);
    let part = st(a, 1);
    let v = st(a, 2);
    let Ok(mut u) = url::Url::parse(&href) else { return Ok(HostValue::Null) };
    use url::quirks as q;
    match part.as_str() {
        "protocol" => {
            let _ = q::set_protocol(&mut u, &v);
        }
        "username" => {
            let _ = q::set_username(&mut u, &v);
        }
        "password" => {
            let _ = q::set_password(&mut u, &v);
        }
        "host" => {
            let _ = q::set_host(&mut u, &v);
        }
        "hostname" => {
            let _ = q::set_hostname(&mut u, &v);
        }
        "port" => {
            let _ = q::set_port(&mut u, &v);
        }
        "pathname" => q::set_pathname(&mut u, &v),
        "search" => q::set_search(&mut u, &v),
        "hash" => q::set_hash(&mut u, &v),
        _ => {}
    }
    Ok(jstr(u.as_str()))
});

native!(n_pointer_lock, |a| {
    let on = a.value(0).map(|v| v.truthy()).unwrap_or(false);
    with_page(|p| {
        if p.pl_active != on {
            p.pl_active = on;
            p.emit(crate::page::PageEvent::PointerLock(on));
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_open_tab, |a| {
    let u = st(a, 0);
    let ok = with_page(|p| {
        if !p.user_gesture {
            return false; // popup blocker
        }
        p.user_gesture = false;
        match p.doc.resolve_url(&u) {
            Some(abs) if matches!(abs.scheme(), "http" | "https") => {
                // HeronBlock: no new tabs to ad and tracking hosts.
                if crate::ext::blocks_popup(&abs, &p.doc.url, Some(p.tab_id)) {
                    return false;
                }
                p.emit(crate::page::PageEvent::OpenTab(abs.to_string()));
                true
            }
            _ => false,
        }
    })
    .unwrap_or(false);
    Ok(HostValue::from(ok))
});

// Debugging switch for the prelude: is the OHB_TRACE_MISSING env var set?
native!(n_trace_missing, |_a| { Ok(HostValue::from(std::env::var_os("OHB_TRACE_MISSING").is_some())) });

// Debugging switch for the prelude: is OHB_SLOW_LISTENERS set (log slow event listeners)?
native!(n_trace_slow, |_a| { Ok(HostValue::from(std::env::var_os("OHB_SLOW_LISTENERS").is_some())) });

native!(n_css_supports, |a| {
    let q = st(a, 0);
    Ok(HostValue::from(crate::css::supports_query(&q)))
});

native!(n_matches_syntax, |a| {
    // (syntax, value text) -> does the value match a registered property syntax?
    let syn = st(a, 0);
    let v = crate::css::tokenizer::parse_cvs(&st(a, 1));
    Ok(HostValue::from(crate::css::pv::matches_syntax(&syn, &v)))
});

native!(n_css_longhand_names, |_a| {
    // All supported longhands, as getComputedStyle enumerates them: standard
    // names alphabetically, then vendor-prefixed ones.
    let mut v: Vec<String> = crate::css::pv::db::with_db(|db| db.props.keys().map(|k| k.to_string()).collect());
    v.retain(|n| !n.starts_with("--"));
    v.sort_by(|a, b| (a.starts_with('-'), a).cmp(&(b.starts_with('-'), b)));
    Ok(jstrs(v))
});

native!(n_css_known, |a| { Ok(HostValue::from(crate::css::known_property(&st(a, 0)) || crate::css::pv::is_known(&st(a, 0)))) });

thread_local! {
    static CSS_BLOCKS: std::cell::RefCell<std::collections::HashMap<String, std::rc::Rc<Vec<crate::css::pv::block::Entry>>>> = Default::default();
}

fn css_block_of(text: &str) -> Vec<crate::css::pv::block::Entry> {
    if let Some(b) = CSS_BLOCKS.with(|c| c.borrow().get(text).cloned()) {
        return (*b).clone();
    }
    let b = crate::css::pv::block::parse_block(text, &crate::css::known_property);
    css_block_remember(text, &b);
    b
}

fn css_block_remember(text: &str, b: &[crate::css::pv::block::Entry]) {
    CSS_BLOCKS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 512 {
            c.clear();
        }
        c.insert(text.to_string(), std::rc::Rc::new(b.to_vec()));
    });
}

native!(n_css_block, |a| {
    // (text, op, prop, value, priority): CSSOM operations on a declaration block.
    use crate::css::pv::block;
    let text = st(a, 0);
    let op = st(a, 1);
    let prop = st(a, 2);
    let mut es = css_block_of(&text);
    let commit = |es: &[block::Entry]| -> String {
        let t = block::serialize(es);
        css_block_remember(&t, es);
        block::remember_precise(&t, es);
        t
    };
    Ok(match op.as_str() {
        "get" => jstr(&block::get(&es, &prop)),
        "prio" => HostValue::from(block::priority(&es, &prop)),
        "names" => jstrs(es.iter().map(|e| e.name.clone()).collect()),
        "text" => jstr(&block::serialize(&es)),
        "set" => {
            let val = st(a, 3);
            let imp = st(a, 4).eq_ignore_ascii_case("important");
            match block::parse_one(&prop, &val, imp, &crate::css::known_property) {
                Some(new) => {
                    block::set(&mut es, new, false);
                    jstr(&commit(&es))
                }
                None => HostValue::Null,
            }
        }
        "remove" => {
            let old = block::get(&es, &prop);
            if block::remove(&mut es, &prop) {
                jstrs(vec![commit(&es), old])
            } else {
                jstrs(vec!["\u{0}".to_string(), old])
            }
        }
        "parse" => {
            // Normalize a whole declaration list (cssText setter).
            let b = block::parse_block(&prop, &crate::css::known_property);
            jstr(&commit(&b))
        }
        _ => HostValue::Null,
    })
});

native!(n_css_decl_ok, |a| {
    // (prop, value): whether a declaration is valid (CSS.supports(prop, value)).
    let prop = st(a, 0);
    let val = st(a, 1);
    let (v, imp) = crate::css::pv::block::strip_important(&val);
    if imp {
        return Ok(HostValue::from(false));
    }
    Ok(HostValue::from(crate::css::pv::block::parse_one(&prop, &v, false, &|n| crate::css::known_property(n) && !crate::css::pv::is_known(n)).is_some()))
});

native!(n_css_color_spec, |a| {
    // The specified-value serialization of a <color> (null if invalid).
    Ok(match crate::css::color::specified_text(&st(a, 0)) {
        Some(t) => HostValue::from(t),
        None => HostValue::Null,
    })
});

native!(n_media, |a| {
    // (query, width?, height?): against the viewport, or a frame's of that size.
    let q = st(a, 0);
    let (w, h) = (num(a, 1), num(a, 2));
    let r = with_page(|p| {
        let mut env = p.media_env();
        if w.is_finite() && h.is_finite() {
            env.width = w as f32;
            env.height = h as f32;
        }
        crate::css::media_matches(&crate::css::tokenizer::parse_cvs(&q), &env)
    })
    .unwrap_or(false);
    Ok(HostValue::from(r))
});

native!(n_frame_viewport, |a| {
    let v = nid(a, 0).and_then(|f| with_page(|p| p.frame_viewport(f))).unwrap_or((0.0, 0.0, 0.0, 0.0));
    Ok(jnums(&[v.0 as f64, v.1 as f64, v.2 as f64, v.3 as f64]))
});

/// The pieces of a script fetch: request and Fetch options.
fn script_request(p: &crate::page::Page, a: &mut dyn HostArgs, sync: bool) -> Result<(crate::net::Request, crate::net::cors::Opts), crate::net::NetError> {
    // (url, method, headers[flat], body, credentials, mode, redirect, cache, forcePreflight)
    let o = if sync { 0 } else { 1 };
    let url_s = st(a, o);
    let method = st(a, o + 1);
    let headers_v = a.value(o + 2).unwrap_or(HostValue::Null);
    let body: Option<Vec<u8>> = match a.value(o + 3).unwrap_or(HostValue::Null) {
        HostValue::Bytes(b) => Some(b),
        v if v.is_nullish() => None,
        v => Some(v.string().into_bytes()),
    };
    let cred = st(a, o + 4);
    let mode = st(a, o + 5);
    let redirect = st(a, o + 6);
    let cache = st(a, o + 7);
    let force_preflight = a.value(o + 8).map(|v| v.truthy()).unwrap_or(false);
    let referrer = opt_st(a, o + 9).unwrap_or_else(|| "client".into());
    let referrer_policy = opt_st(a, o + 10).unwrap_or_default();
    let target = p.doc.resolve_url(&url_s).ok_or_else(|| crate::net::NetError::InvalidUrl(url_s.clone()))?;
    if target.scheme() == "file" && p.doc.url.scheme() != "file" {
        return Err(crate::net::NetError::Blocked("file access".into()));
    }
    let mut req = crate::net::Request::get(target, crate::net::RequestKind::Fetch, Some(p.doc.url.clone()));
    req.method = if method.is_empty() { "GET".into() } else { method };
    if let Some(arr) = headers_v.as_array() {
        for kv in arr.chunks(2) {
            if kv.len() == 2 {
                req.headers.push((kv[0].string(), kv[1].string()));
            }
        }
    }
    req.body = body;
    let mut opts = crate::net::cors::Opts::parse(p.doc.url.clone(), &mode, &cred, &redirect, &cache);
    opts.force_preflight = force_preflight;
    opts.referrer = referrer;
    opts.referrer_policy = referrer_policy;
    Ok((req, opts))
}

native!(n_fetch, |a| {
    // (id, url, method, headers[flat], body, credentials, mode, redirect, cache, forcePreflight)
    let id = num(a, 0) as u64;
    with_page(|p| {
        let (req, mut opts) = match script_request(p, a, false) {
            Ok(x) => x,
            Err(e) => {
                p.fetch_results.push((id, Err(e)));
                return;
            }
        };
        let tx = p.tx.clone();
        let gen = p.nav_gen;
        // Script fetches stream: the head resolves fetch(), chunks feed response.body.
        if opts.mode != crate::net::cors::Mode::Media {
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            p.fetch_streams.insert(id, crate::page::FetchStream { cancel: cancel.clone(), ..Default::default() });
            let stx = tx.clone();
            opts.stream = Some(crate::net::cors::StreamTo(std::sync::Arc::new(move |ev| {
                use crate::net::cors::StreamOut;
                match ev {
                    StreamOut::Head { status, status_text, url, headers, rtype, redirected } => {
                        let h = crate::page::FetchHead { status, status_text: status_text.to_string(), url: url.to_string(), headers: headers.to_vec(), rtype, redirected };
                        let _ = stx.send(PageMsg::Net(gen, NetMsg::FetchHead(id, Box::new(h))));
                    }
                    StreamOut::Chunk(b) => {
                        if !b.is_empty() {
                            let _ = stx.send(PageMsg::Net(gen, NetMsg::FetchChunk(id, b.to_vec())));
                        }
                    }
                }
                !cancel.load(std::sync::atomic::Ordering::SeqCst)
            })));
        }
        let dbg = std::env::var_os("OHB_DEBUG_NET").map(|_| (req.url.to_string(), crate::net::wall_secs()));
        crate::net::fetch_cors_async(req, opts, move |r| {
            if let Some((u, t0)) = dbg {
                eprintln!("[net] @{:.3} fetch() {} {} ({:.0}ms)", crate::net::wall_secs(), u, if r.is_ok() { "ok" } else { "ERR" }, (crate::net::wall_secs() - t0) * 1000.0);
            }
            let _ = tx.send(PageMsg::Net(gen, NetMsg::Fetch(id, r)));
        });
    });
    Ok(HostValue::Undefined)
});

native!(n_fetch_read, |a| {
    // A streamed body: [1, bytes] (new data), [0] (none yet), [2] (end), [3] (network error).
    let id = num(a, 0) as u64;
    Ok(with_page(|p| {
        let Some(fs) = p.fetch_streams.get_mut(&id) else { return HostValue::Array(vec![HostValue::from(2)]) };
        fs.streamed = true;
        if !fs.buf.is_empty() {
            return HostValue::Array(vec![HostValue::from(1), HostValue::Bytes(std::mem::take(&mut fs.buf))]);
        }
        match fs.done {
            Some(ok) => {
                p.fetch_streams.remove(&id);
                p.fetch_bodies.take(id);
                HostValue::Array(vec![HostValue::from(if ok { 2 } else { 3 })])
            }
            None => HostValue::Array(vec![HostValue::from(0)]),
        }
    })
    .unwrap_or_else(|| HostValue::Array(vec![HostValue::from(2)])))
});

native!(n_fetch_want, |a| {
    // Script waits for more of a streamed body (__ohb_fetchChunk when it comes).
    let id = num(a, 0) as u64;
    with_page(|p| {
        if let Some(fs) = p.fetch_streams.get_mut(&id) {
            fs.want = true;
            fs.streamed = true;
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_fetch_abort, |a| {
    // Abort a script fetch in flight (the connection is closed).
    let id = num(a, 0) as u64;
    with_page(|p| {
        if let Some(fs) = p.fetch_streams.remove(&id) {
            fs.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        p.fetch_bodies.take(id);
    });
    Ok(HostValue::Undefined)
});

native!(n_fetch_sync, |a| {
    // Blocking fetch (synchronous XHR): (url, method, headers[flat], body, credentials, mode, redirect, cache, forcePreflight)
    //   -> [status, statusText, url, headers[flat], body bytes, type, redirected]
    let r = with_page(|p| script_request(p, a, true)).unwrap_or_else(|| Err(crate::net::NetError::Blocked("no page".into())));
    let r = r.and_then(|(req, opts)| crate::net::cors::fetch(req, &opts));
    match r {
        Ok(o) => {
            let mut flat = Vec::new();
            for (k, v) in &o.resp.headers {
                flat.push(HostValue::String(k.to_ascii_lowercase()));
                flat.push(HostValue::String(v.clone()));
            }
            Ok(HostValue::Array(vec![
                HostValue::from(o.resp.status as f64),
                HostValue::String(o.resp.status_text.clone()),
                HostValue::String(o.resp.url.to_string()),
                HostValue::Array(flat),
                HostValue::Bytes(o.resp.body.as_ref().clone()),
                jstr(o.rtype),
                HostValue::from(o.redirected),
            ]))
        }
        Err(e) => Err(HostError::type_error(format!("Failed to fetch: {e}"))),
    }
});

native!(n_script_inserted, |a| {
    if let Some(id) = nid(a, 0) {
        with_page(|p| {
            after_insert(p, &[id]);
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_img_info, |a| {
    // [naturalWidth, naturalHeight, complete(0/1)]
    let id = nid(a, 0);
    let v = id
        .and_then(|id| {
            with_page(|p| {
                let (u, _) = p.node_img_url(id)?;
                Some(match p.res.images.get(&u) {
                    Some(crate::page::resources::ImgState::Ready(i)) => [(i.natural().0) as f64, (i.natural().1) as f64, 1.0],
                    Some(crate::page::resources::ImgState::Failed) => [0.0, 0.0, 1.0],
                    _ => [0.0, 0.0, 0.0],
                })
            })
            .flatten()
        })
        .unwrap_or([0.0, 0.0, 1.0]);
    Ok(jnums(&v))
});

native!(n_img_select, |a| {
    // (img, viewportWidth?, viewportHeight?, renderedWidth?) -> the selected
    // image source URL (unresolved; '' when none).
    let id = nid(a, 0);
    let (w, h, aw) = (num(a, 1), num(a, 2), num(a, 3));
    let r = id
        .and_then(|id| {
            with_page(|p| {
                let mut env = p.media_env();
                if w.is_finite() && h.is_finite() {
                    env.width = w as f32;
                    env.height = h as f32;
                }
                crate::page::resources::select_image_source(&p.doc, id, &env, if aw.is_finite() { Some(aw as f32) } else { None }).map(|x| x.0)
            })
            .flatten()
        })
        .unwrap_or_default();
    Ok(HostValue::from(r))
});

native!(n_form_submit, |a| {
    // (form, submitter, entries[flat]?, multipartBody?, multipartType?)
    let submitter = nid(a, 1);
    let entries = a.value(2).ok().and_then(|v| v.as_array().map(|arr| arr.chunks(2).filter(|kv| kv.len() == 2).map(|kv| (kv[0].string(), kv[1].string())).collect::<Vec<_>>()));
    let body = match (a.value(3).unwrap_or(HostValue::Null), opt_st(a, 4)) {
        (HostValue::Bytes(b), Some(t)) => Some((b, t)),
        _ => None,
    };
    let action = opt_st(a, 5).and_then(|u| url::Url::parse(&u).ok());
    if let Some(id) = nid(a, 0) {
        with_page(|p| {
            if p.doc.is_connected(id) || p.frame_of_node(id).is_some() {
                p.submit_form(id, submitter, entries.map(|e| crate::page::FormEntries { pairs: e, multipart: body, action }))
            }
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_adopt, |a| {
    let id = nid(a, 0);
    let owner = nid(a, 1);
    if let (Some(id), Some(o)) = (id, owner) {
        with_page(|p| {
            if p.doc.is_document(o) {
                p.doc.set_owner_tree(id, o);
            }
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_request_submit, |a| {
    let f = nid(a, 0);
    let s = nid(a, 1);
    if let Some(f) = f {
        with_page(|p| p.request_submit(f, s));
    }
    Ok(HostValue::Undefined)
});

native!(n_pre_activate, |a| {
    // Returns [[id, wasChecked], …] or null.
    let id = nid(a, 0);
    let prev = id.and_then(|id| with_page(|p| p.pre_activate(id)).flatten());
    match prev {
        Some(v) => {
            let flat: Vec<HostValue> = v.into_iter().flat_map(|(n, b)| [HostValue::from(n as f64), HostValue::from(b)]).collect();
            Ok(HostValue::Array(flat))
        }
        None => Ok(HostValue::Null),
    }
});

native!(n_finish_activation, |a| {
    let id = nid(a, 0);
    let canceled = boolean(a, 2, false);
    let mut prev = Vec::new();
    if let HostValue::Array(arr) = a.value(1)? {
        for nb in arr.chunks(2) {
            if nb.len() == 2 {
                prev.push((nb[0].number() as usize, nb[1].truthy()));
            }
        }
    }
    // From script (`click()`), the caller fires input/change itself, synchronously.
    let from_script = boolean(a, 3, false);
    let changed = id.and_then(|id| with_page(|p| p.finish_activation_ex(id, prev, canceled, !from_script))).unwrap_or(false);
    Ok(HostValue::from(changed))
});


// ------------------------------------------------------------ canvas 2D

fn css_color(s: &str) -> Option<crate::css::values::Color> {
    let cvs = crate::css::tokenizer::parse_cvs(s);
    let first = cvs.iter().find(|c| !c.is_ws())?;
    crate::css::values::parse_color(first).map(|cv| cv.resolve(crate::css::values::Color::rgb(0, 0, 0)))
}

/// Style array: ['c', color] | ['l', x0,y0,x1,y1, off,color,…] | ['r', x0,y0,r0,x1,y1,r1, …] | ['p', nodeId, repeat]
fn js_style(v: &HostValue) -> crate::page::canvas::Style {
    use crate::page::canvas::Style;
    let black = Style::Color(crate::css::values::Color::rgb(0, 0, 0));
    let o = match v.as_array() {
        Some(o) => o,
        None => return black,
    };
    let len = o.len();
    let getn = |i: usize| -> f32 { o.get(i).map(|x| x.number() as f32).unwrap_or(f32::NAN) };
    let gets = |i: usize| -> String { o.get(i).map(|x| x.string()).unwrap_or_default() };
    let stops = |from: usize| -> Vec<(f32, crate::css::values::Color)> {
        let mut st = Vec::new();
        let mut i = from;
        while i + 1 < len {
            if let Some(cc) = css_color(&gets(i + 1)) {
                st.push((getn(i), cc));
            }
            i += 2;
        }
        st.sort_by(|a, b| a.0.total_cmp(&b.0));
        st
    };
    match gets(0).as_str() {
        "c" => css_color(&gets(1)).map(Style::Color).unwrap_or(black),
        "l" if len >= 5 => Style::Linear([getn(1), getn(2), getn(3), getn(4)], stops(5)),
        "r" if len >= 7 => Style::Radial([getn(1), getn(2), getn(3), getn(4), getn(5), getn(6)], stops(7)),
        "p" => {
            let src = getn(1);
            let rep = gets(2);
            let img = if src.is_finite() && src >= 0.0 { canvas_source(src as usize) } else { None };
            match img {
                Some(i) => Style::Pattern(i, rep != "no-repeat" && rep != "repeat-y", rep != "no-repeat" && rep != "repeat-x"),
                None => black,
            }
        }
        _ => black,
    }
}

/// Pixels of an <img> or <canvas> element usable as a drawImage/pattern source.
fn canvas_source(n: usize) -> Option<std::sync::Arc<crate::paint::ImageData>> {
    with_page(|p| {
        if let Some(c) = p.res.canvases.get(&n) {
            return Some(c.clone());
        }
        if !p.doc.valid(n) {
            return None;
        }
        // Resolved against the image's own document (a frame's `new Image()`).
        if let Some((u, _)) = p.node_img_url(n) {
            if let Some(i) = p.res.get(&u) {
                return Some(i);
            }
        }
        let view = crate::page::resources::ResView { res: &p.res };
        crate::layout::ResourceView::image_for(&view, &p.doc, n)
    })
    .flatten()
}

fn js_params(a: &mut dyn HostArgs, style_i: usize) -> Result<crate::page::canvas::DrawParams, HostError> {
    let style = js_style(&a.value(style_i)?);
    let alpha = num(a, style_i + 1) as f32;
    let t = f32s(a, style_i + 2)?;
    let comp = st(a, style_i + 3);
    let clip = if a.is_object(style_i + 4) { Some(f32s(a, style_i + 4)?) } else { None };
    let t = if t.len() == 6 { tiny_skia::Transform::from_row(t[0], t[1], t[2], t[3], t[4], t[5]) } else { tiny_skia::Transform::identity() };
    Ok(crate::page::canvas::DrawParams { style, alpha: if alpha.is_finite() { alpha } else { 1.0 }, t, composite: crate::page::canvas::composite_mode(&comp), clip })
}

/// A canvas `font` string, parsed once: the same font keeps the same
/// family list, so the text caches (keyed by it) hit instead of filling up
/// with a fresh list per call (fingerprinting scripts measure text in a
/// tight loop).
fn canvas_font(font: &str) -> (std::rc::Rc<Vec<String>>, u16, crate::css::style::FontStyle, f32) {
    thread_local! {
        static FONTS: std::cell::RefCell<std::collections::HashMap<String, (std::rc::Rc<Vec<String>>, u16, crate::css::style::FontStyle, f32)>> = Default::default();
    }
    if let Some(f) = FONTS.with(|m| m.borrow().get(font).cloned()) {
        return f;
    }
    let f = crate::css::style::parse_canvas_font(font).unwrap_or_else(|| (std::rc::Rc::new(vec!["sans-serif".into()]), 400, crate::css::style::FontStyle::Normal, 10.0));
    FONTS.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > 512 {
            m.clear();
        }
        m.insert(font.to_string(), f.clone());
    });
    f
}

native!(n_canvas, |a| {
    use crate::page::canvas as cv;
    let op = st(a, 0);
    let id = num(a, 1);
    let id = if id.is_finite() && id >= 0.0 { id as usize } else { usize::MAX };
    match op.as_str() {
        "size" => {
            let (w, h) = (num(a, 2).max(0.0) as u32, num(a, 3).max(0.0) as u32);
            with_page(|p| {
                p.res.canvases.insert(id, cv::new_canvas(w, h));
                p.need_paint = true;
            });
        }
        "fill" => {
            let ops = f32s(a, 2)?;
            let even_odd = boolean(a, 3, false);
            let params = js_params(a, 4)?;
            with_page(|p| {
                if let Some(img) = p.res.canvases.get_mut(&id) {
                    cv::fill(img, &ops, even_odd, &params);
                    p.need_paint = true;
                }
            });
        }
        "stroke" => {
            let ops = f32s(a, 2)?;
            let params = js_params(a, 3)?;
            let w = num(a, 8) as f32;
            let cap = num(a, 9) as u8;
            let join = num(a, 10) as u8;
            let miter = num(a, 11) as f32;
            let dash = f32s(a, 12)?;
            with_page(|p| {
                if let Some(img) = p.res.canvases.get_mut(&id) {
                    cv::stroke(img, &ops, if w.is_finite() { w } else { 1.0 }, cap, join, if miter.is_finite() { miter } else { 10.0 }, &dash, &params);
                    p.need_paint = true;
                }
            });
        }
        "text" => {
            let text = st(a, 2);
            let (x, y) = (num(a, 3) as f32, num(a, 4) as f32);
            let font = st(a, 5);
            let align = num(a, 6) as u8;
            let baseline = num(a, 7) as u8;
            let max_w = num(a, 8) as f32;
            let stroke_w = num(a, 9) as f32;
            let params = js_params(a, 10)?;
            let f = canvas_font(&font);
            with_page(|p| {
                if let Some(mut img) = p.res.canvases.remove(&id) {
                    cv::draw_text(&mut img, &mut p.fonts, &text, x, y, (&f.0, f.1, f.2, f.3), align, baseline, if max_w.is_finite() { Some(max_w) } else { None }, if stroke_w.is_finite() { Some(stroke_w) } else { None }, &params);
                    p.res.canvases.insert(id, img);
                    p.need_paint = true;
                }
            });
        }
        "measure" => {
            let text = st(a, 2);
            let font = st(a, 3);
            let f = canvas_font(&font);
            let m = with_page(|p| cv::measure_text(&mut p.fonts, &text, (&f.0, f.1, f.2, f.3))).unwrap_or((0.0, 0.0, 0.0));
            return Ok(jnums(&[m.0 as f64, m.1 as f64, m.2 as f64]));
        }
        "image" => {
            let src = num(a, 2);
            let sr = f32s(a, 3)?;
            let dr = f32s(a, 4)?;
            let alpha = num(a, 5) as f32;
            let t = f32s(a, 6)?;
            let comp = st(a, 7);
            let smooth = boolean(a, 8, true);
            let clip = if a.is_object(9) { Some(f32s(a, 9)?) } else { None };
            let srcimg = if src.is_finite() && src >= 0.0 { canvas_source(src as usize) } else { None };
            if let (Some(si), true, true, true) = (srcimg, sr.len() == 4 || sr.is_empty(), dr.len() == 4, t.len() == 6) {
                let s4 = if sr.len() == 4 { [sr[0], sr[1], sr[2], sr[3]] } else { [0.0, 0.0, si.natural().0, si.natural().1] };
                let tt = tiny_skia::Transform::from_row(t[0], t[1], t[2], t[3], t[4], t[5]);
                with_page(|p| {
                    if let Some(img) = p.res.canvases.get_mut(&id) {
                        cv::draw_image(img, &si, s4, [dr[0], dr[1], dr[2], dr[3]], tt, if alpha.is_finite() { alpha } else { 1.0 }, cv::composite_mode(&comp), smooth, &clip);
                        p.need_paint = true;
                    }
                });
            }
        }
        "dims" => {
            // Natural size of an image/canvas source.
            let src = num(a, 2);
            let d = if src.is_finite() && src >= 0.0 { canvas_source(src as usize).map(|i| i.natural()) } else { None };
            return Ok(match d {
                Some((w, h)) => jnums(&[w as f64, h as f64]),
                None => HostValue::Null,
            });
        }
        "get" => {
            let (x, y) = (num(a, 2) as i32, num(a, 3) as i32);
            let (w, h) = (num(a, 4).max(0.0) as u32, num(a, 5).max(0.0) as u32);
            if (w as u64) * (h as u64) > 16_000_000 {
                return Ok(HostValue::Null);
            }
            let bytes = with_page(|p| p.res.canvases.get(&id).map(|img| cv::get_image_data(img, x, y, w, h))).flatten().unwrap_or_else(|| vec![0; (w * h * 4) as usize]);
            return Ok(HostValue::Bytes(bytes));
        }
        "put" => {
            let data = a.value(2)?;
            let (w, h) = (num(a, 3).max(0.0) as u32, num(a, 4).max(0.0) as u32);
            let (x, y) = (num(a, 5) as i32, num(a, 6) as i32);
            let bytes = match data {
                HostValue::Bytes(b) => Some(b),
                HostValue::Array(v) => Some(v.iter().map(|x| x.number() as u8).collect()),
                _ => None,
            };
            if let Some(bytes) = bytes {
                with_page(|p| {
                    if let Some(img) = p.res.canvases.get_mut(&id) {
                        cv::put_image_data(img, &bytes, w, h, x, y);
                        p.need_paint = true;
                    }
                });
            }
        }
        "png" => {
            let png = with_page(|p| p.res.canvases.get(&id).and_then(|i| cv::to_png(i))).flatten();
            let url = match png {
                Some(b) => format!("data:image/png;base64,{}", crate::net::base64_encode(&b)),
                None => "data:,".to_string(),
            };
            return Ok(HostValue::String(url));
        }
        _ => {}
    }
    Ok(HostValue::Undefined)
});

native!(n_define_element, |a| {
    let name = st(a, 0);
    with_page(|p| {
        // Upgraded elements each record their `:defined` state change.
        p.doc.defined_custom.insert(name);
    });
    Ok(HostValue::Undefined)
});

native!(n_quirks, |a| {
    Ok(HostValue::from(with_page(|p| p.doc.quirks).unwrap_or(false)))
});

native!(n_click_default, |a| {
    if let Some(id) = nid(a, 0) {
        with_page(|p| p.default_click(id, crate::page::Mods::default(), 0));
    }
    Ok(HostValue::Undefined)
});

thread_local!(static DOC_WRITES: std::cell::RefCell<std::collections::HashMap<NodeId, (String, Vec<NodeId>, bool)>> = std::cell::RefCell::new(std::collections::HashMap::new()));

native!(n_doc_write, |a| {
    let html = st(a, 0);
    let after = nid(a, 1);
    with_page(|p| {
        let parent_ctx = after.and_then(|s| p.doc.parent(s)).or(p.doc.body()).unwrap_or(DOCUMENT);
        // Consecutive writes from one script form one stream of markup
        // ("<table>", then "<tr>..." rows, then "</table>"): the script's
        // output so far is parsed again as a whole and replaces what the
        // earlier writes inserted, in order, right after the script. Once
        // the output holds a script (which must not run twice) or grows
        // large, later writes are parsed on their own and appended after it.
        let mut html = html;
        let mut anchor = after;
        if let Some(s) = after.filter(|&s| p.doc.parent(s).is_some()) {
            let par = p.doc.parent(s).unwrap();
            DOC_WRITES.with(|m| {
                let mut m = m.borrow_mut();
                let e = m.entry(s).or_insert_with(|| (String::new(), Vec::new(), false));
                let intact = e.1.iter().all(|&k| p.doc.parent(k) == Some(par));
                if !intact {
                    *e = (String::new(), Vec::new(), true);
                }
                if e.2 {
                    if let Some(&last) = e.1.last() {
                        anchor = Some(last);
                    }
                } else {
                    for &k in &e.1 {
                        p.doc.detach(k);
                    }
                    e.1.clear();
                    e.0.push_str(&html);
                    html = e.0.clone();
                }
            });
        }
        let frag = crate::html::parse_fragment(&mut p.doc, parent_ctx, &html);
        let kids = p.doc.children(frag).to_vec();
        if let Some(s) = after {
            let has_script = kids.iter().any(|&k| p.doc.is_tag(k, "script") || p.doc.descendants(k).any(|d| p.doc.is_tag(d, "script")));
            DOC_WRITES.with(|m| {
                if let Some(e) = m.borrow_mut().get_mut(&s) {
                    e.1.extend(kids.iter().copied());
                    if has_script || e.0.len() > 256 * 1024 {
                        e.2 = true;
                    }
                }
            });
        }
        match anchor {
            Some(s) if p.doc.parent(s).is_some() => {
                let par = p.doc.parent(s).unwrap();
                let next = p.doc.next_sibling(s);
                p.doc.insert_before(par, frag, next);
            }
            _ => {
                if let Some(b) = p.doc.body() {
                    p.doc.append_child(b, frag);
                }
            }
        }
        after_insert(p, &kids);
        // Parser-inserted about:blank iframes have their document at once.
        for k in kids {
            let mut list = vec![k];
            list.extend(p.doc.descendants(k));
            for d in list {
                if p.doc.is_tag(d, "iframe") && p.doc.attr(d, "srcdoc").is_none() && p.doc.attr(d, "src").map(|s| s.trim().is_empty()).unwrap_or(true) {
                    p.js_tasks.push(crate::page::JsTask::Dispatch { target: d, ty: "load".into(), init: crate::page::EventInit::default(), then: crate::page::DefaultAction::None });
                }
            }
        }
    });
    Ok(HostValue::Undefined)
});

native!(n_ready_state, |a| {
    let s = with_page(|p| if p.load_fired { "complete" } else if p.dcl_fired { "interactive" } else { "loading" }).unwrap_or("complete");
    Ok(jstr(s))
});

native!(n_btoa, |a| {
    let s = st(a, 0);
    if s.chars().any(|ch| ch as u32 > 255) {
        return Ok(HostValue::Null);
    }
    let bytes: Vec<u8> = s.chars().map(|ch| ch as u8).collect();
    Ok(jstr(&crate::net::base64_encode(&bytes)))
});

native!(n_atob, |a| {
    let s = st(a, 0);
    match crate::net::base64_decode(s.as_bytes()) {
        Some(b) => Ok(jstr(&b.iter().map(|&x| x as char).collect::<String>())),
        None => Ok(HostValue::Null),
    }
});

native!(n_fetch_body, |a| {
    // The raw body of a finished fetch (consumed once).
    let id = num(a, 0) as u64;
    let b = with_page(|p| p.fetch_bodies.take(id)).flatten();
    Ok(match b {
        Some(b) => HostValue::Bytes(b.as_ref().clone()),
        None => HostValue::Null,
    })
});

native!(n_fetch_drop, |a| {
    let id = num(a, 0) as u64;
    with_page(|p| {
        // The body is not wanted (the request keeps running until done or aborted).
        if let Some(fs) = p.fetch_streams.get_mut(&id) {
            fs.discard = true;
            fs.buf = Vec::new();
        }
        p.fetch_bodies.take(id)
    });
    Ok(HostValue::Undefined)
});

native!(n_utf8_encode, |a| {
    Ok(HostValue::Bytes(st(a, 0).into_bytes()))
});

native!(n_zip, |a| {
    // (bytes, format, compress) -> bytes, or null for malformed input.
    use std::io::{Read, Write};
    let bytes = match a.value(0)? {
        HostValue::Bytes(b) => b,
        HostValue::Array(v) => v.iter().map(|x| x.number() as u8).collect(),
        _ => Vec::new(),
    };
    let format = st(a, 1);
    let compress = boolean(a, 2, true);
    let lvl = flate2::Compression::default();
    let out: Option<Vec<u8>> = if compress {
        match format.as_str() {
            "gzip" => { let mut e = flate2::write::GzEncoder::new(Vec::new(), lvl); e.write_all(&bytes).ok().and_then(|_| e.finish().ok()) }
            "deflate" => { let mut e = flate2::write::ZlibEncoder::new(Vec::new(), lvl); e.write_all(&bytes).ok().and_then(|_| e.finish().ok()) }
            _ => { let mut e = flate2::write::DeflateEncoder::new(Vec::new(), lvl); e.write_all(&bytes).ok().and_then(|_| e.finish().ok()) }
        }
    } else {
        let mut o = Vec::new();
        let ok = match format.as_str() {
            "gzip" => flate2::read::MultiGzDecoder::new(&bytes[..]).read_to_end(&mut o).is_ok(),
            "deflate" => flate2::read::ZlibDecoder::new(&bytes[..]).read_to_end(&mut o).is_ok(),
            _ => flate2::read::DeflateDecoder::new(&bytes[..]).read_to_end(&mut o).is_ok(),
        };
        ok.then_some(o)
    };
    Ok(match out {
        Some(b) => HostValue::Bytes(b),
        None => HostValue::Null,
    })
});

// Streaming compression contexts: zipOpen(format, compress) -> id or null;
// zipPush(id, bytes) -> [bytes, error]; zipEnd(id) -> bytes or null; zipClose(id).
native!(n_zip_open, |a| {
    Ok(match super::zipstream::open(&st(a, 0), boolean(a, 1, true)) {
        Some(id) => HostValue::Number(id as f64),
        None => HostValue::Null,
    })
});

native!(n_zip_push, |a| {
    let id = num(a, 0) as u64;
    let bytes = bytes_of(a.value(1)?);
    let f = super::zipstream::push(id, &bytes);
    Ok(HostValue::Array(vec![HostValue::Bytes(f.out), HostValue::Bool(f.error)]))
});

native!(n_zip_end, |a| {
    Ok(match super::zipstream::finish(num(a, 0) as u64) {
        Some(b) => HostValue::Bytes(b),
        None => HostValue::Null,
    })
});

native!(n_zip_close, |a| {
    super::zipstream::close(num(a, 0) as u64);
    Ok(HostValue::Undefined)
});

fn bytes_of(v: HostValue) -> Vec<u8> {
    match v {
        HostValue::Bytes(b) => b,
        HostValue::Array(v) => v.iter().map(|x| x.number() as u8).collect(),
        _ => Vec::new(),
    }
}

/// Run `dec` over `src` into a string; None when `fatal` and the input is malformed.
fn decode_with(dec: &mut encoding_rs::Decoder, src: &[u8], fatal: bool, last: bool) -> Option<String> {
    let mut out = String::new();
    let mut src = src;
    loop {
        if fatal {
            let need = dec.max_utf8_buffer_length_without_replacement(src.len()).unwrap_or(src.len() * 3 + 16);
            out.reserve(need.max(16));
            let (r, read) = dec.decode_to_string_without_replacement(src, &mut out, last);
            src = &src[read..];
            match r {
                encoding_rs::DecoderResult::InputEmpty => return Some(out),
                encoding_rs::DecoderResult::OutputFull => continue,
                encoding_rs::DecoderResult::Malformed(_, _) => return None,
            }
        } else {
            let need = dec.max_utf8_buffer_length(src.len()).unwrap_or(src.len() * 3 + 16);
            out.reserve(need.max(16));
            let (r, read, _) = dec.decode_to_string(src, &mut out, last);
            src = &src[read..];
            match r {
                encoding_rs::CoderResult::InputEmpty => return Some(out),
                encoding_rs::CoderResult::OutputFull => continue,
            }
        }
    }
}

fn new_decoder(label: &str, ignore_bom: bool) -> encoding_rs::Decoder {
    let enc = crate::html::encoding::for_label(label).unwrap_or(encoding_rs::UTF_8);
    if ignore_bom {
        enc.new_decoder_without_bom_handling()
    } else {
        enc.new_decoder_with_bom_removal()
    }
}

thread_local! {
    static TEXT_DECODERS: std::cell::RefCell<(u32, std::collections::HashMap<u32, encoding_rs::Decoder>)> = std::cell::RefCell::new((0, std::collections::HashMap::new()));
}

native!(n_text_decode, |a| {
    // (bytes, label, fatal, ignoreBOM) -> string; null when `fatal` and the input is malformed.
    let bytes = bytes_of(a.value(0)?);
    let label = st(a, 1);
    let fatal = boolean(a, 2, false);
    let ignore_bom = boolean(a, 3, false);
    let mut dec = new_decoder(&label, ignore_bom);
    Ok(match decode_with(&mut dec, &bytes, fatal, true) {
        Some(s) => HostValue::String(s),
        None => HostValue::Null,
    })
});

native!(n_decode_doc, |a| {
    // (bytes, charsetLabel|null, html) -> [text, encoding name] (encoding::decode_fetched_doc).
    let bytes = bytes_of(a.value(0)?);
    let label = opt_st(a, 1);
    let html = boolean(a, 2, false);
    let (text, enc) = crate::html::encoding::decode_fetched_doc(&bytes, label.as_deref(), html);
    Ok(HostValue::Array(vec![HostValue::String(text), jstr(enc)]))
});

native!(n_digest, |a| {
    // (algorithm "SHA-1"|"SHA-256"|"SHA-384"|"SHA-512", bytes) -> ArrayBuffer
    let alg = match st(a, 0).to_ascii_uppercase().as_str() {
        "SHA-1" => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        "SHA-384" => &ring::digest::SHA384,
        "SHA-512" => &ring::digest::SHA512,
        _ => &ring::digest::SHA256,
    };
    let bytes = bytes_of(a.value(1)?);
    Ok(HostValue::Bytes(ring::digest::digest(alg, &bytes).as_ref().to_vec()))
});


fn hv_to_v(h: HostValue) -> ohb_crypto::V {
    use ohb_crypto::V;
    match h {
        HostValue::Undefined => V::Undefined,
        HostValue::Null => V::Null,
        HostValue::Bool(b) => V::Bool(b),
        HostValue::Number(n) => V::Num(n),
        HostValue::String(s) => V::Str(s),
        HostValue::Array(a) => V::Arr(a.into_iter().map(hv_to_v).collect()),
        HostValue::Object(o) => V::Obj(o.into_iter().map(|(k, v)| (k, hv_to_v(v))).collect()),
        HostValue::Bytes(b) => V::Bytes(b),
    }
}

fn v_to_hv(v: ohb_crypto::V) -> HostValue {
    use ohb_crypto::V;
    match v {
        V::Undefined => HostValue::Undefined,
        V::Null => HostValue::Null,
        V::Bool(b) => HostValue::Bool(b),
        V::Num(n) => HostValue::Number(n),
        V::Str(s) => HostValue::String(s),
        V::Arr(a) => HostValue::Array(a.into_iter().map(v_to_hv).collect()),
        V::Obj(o) => HostValue::Object(o.into_iter().map(|(k, v)| (k, v_to_hv(v))).collect()),
        V::Bytes(b) => HostValue::Bytes(b),
    }
}

native!(n_webcrypto, |a| {
    // (op, args) -> result: Web Crypto operations (crates/ohb-crypto; webcrypto.js).
    let op = st(a, 0);
    let args = hv_to_v(a.value(1)?);
    ohb_crypto::call(&op, &args).map(v_to_hv).map_err(|(n, m)| HostError::error(format!("{n}: {m}")))
});

native!(n_script_force_async, |a| {
    // (id) -> bool, or (id, false): read / clear a script element's "force async" flag.
    let Some(id) = nid(a, 0) else { return Ok(HostValue::Bool(false)) };
    let clear = a.len() > 1;
    Ok(HostValue::Bool(with_page(|p| if clear { p.script_force_async.remove(&id); false } else { p.script_force_async.contains(&id) }).unwrap_or(false)))
});

native!(n_editing_source, |_a| { Ok(HostValue::String(include_str!("editing.js").to_string())) });

native!(n_selection_changed, |a| {
    // (anchorNode, anchorOffset, focusNode, focusOffset) or (null): the DOM selection for painting.
    let an = nid(a, 0);
    let ao = num(a, 1) as usize;
    let fnode = nid(a, 2);
    let fo = num(a, 3) as usize;
    with_page(|p| {
        p.dom_sel = match (an, fnode) {
            (Some(an), Some(f)) => Some((an, ao, f, fo)),
            _ => None,
        };
        if p.dom_sel.is_none() {
            p.sel = None;
        }
        p.sel_gen = p.doc.generation;
        p.need_paint = true;
    });
    Ok(HostValue::Undefined)
});

native!(n_set_design_mode, |a| {
    let on = boolean(a, 0, false);
    with_page(|p| {
        p.design_mode = on;
        p.need_paint = true;
    });
    Ok(HostValue::Undefined)
});

native!(n_encoding_name, |a| {
    // A TextDecoder label -> the encoding's lowercase name (null if unknown).
    Ok(match crate::html::encoding::for_label(&st(a, 0)) {
        Some(e) => jstr(&e.name().to_ascii_lowercase()),
        None => HostValue::Null,
    })
});

native!(n_text_decoder_new, |a| {
    // (label, ignoreBOM) -> handle of a streaming decoder.
    let dec = new_decoder(&st(a, 0), boolean(a, 1, false));
    Ok(HostValue::Number(TEXT_DECODERS.with(|t| {
        let mut t = t.borrow_mut();
        t.0 += 1;
        let h = t.0;
        t.1.insert(h, dec);
        h as f64
    })))
});

native!(n_text_decode_chunk, |a| {
    // (handle, bytes, fatal, last) -> string (null on a fatal error). The
    // decoder is dropped after the last chunk or an error.
    let h = num(a, 0) as u32;
    let bytes = bytes_of(a.value(1)?);
    let fatal = boolean(a, 2, false);
    let last = boolean(a, 3, false);
    let r = TEXT_DECODERS.with(|t| {
        let mut t = t.borrow_mut();
        let r = t.1.get_mut(&h).map(|d| decode_with(d, &bytes, fatal, last));
        if last || matches!(r, Some(None)) {
            t.1.remove(&h);
        }
        r.flatten()
    });
    Ok(match r {
        Some(s) => HostValue::String(s),
        None => HostValue::Null,
    })
});

native!(n_selector_valid, |a| {
    // Whether a stylesheet would accept this selector list (`nested`: as a
    // nested style rule's, relative to a parent rule).
    let text = st(a, 0);
    if boolean(a, 1, false) {
        let parent = crate::css::selector::parse_selector_list_str(":root").unwrap_or_default();
        let cvs = crate::css::tokenizer::parse_cvs(&text);
        return Ok(HostValue::Bool(crate::css::selector::parse_selector_list(&cvs, Some(&parent)).is_some()));
    }
    Ok(HostValue::Bool(crate::css::selector::parse_selector_list_str(&text).is_some()))
});

native!(n_media_canon, |a| {
    // The serialized queries of a media query list ("not all" where invalid).
    Ok(jstrs(crate::css::media_canon(&st(a, 0))))
});

native!(n_supports_valid, |a| {
    Ok(HostValue::Bool(crate::css::supports_valid(&st(a, 0))))
});

native!(n_style_overridden, |a| {
    // Whether CSSOM's edited text still stands in for a <style>'s contents.
    let id = nid(a, 0);
    Ok(HostValue::Bool(id.and_then(|id| with_page(|p| p.style_override.contains_key(&id))).unwrap_or(false)))
});

native!(n_set_style_text, |a| {
    // CSSOM changed a <style> sheet's rules: the engine reads this text for it
    // (the element's own text is unchanged) until its contents change.
    let id = nid(a, 0);
    let t = st(a, 1);
    if let Some(id) = id {
        with_page(|p| {
            p.style_override.insert(id, std::rc::Rc::from(t.as_str()));
            // A sheet in a same-origin frame's document: that document changed
            // (its frame restyles at the next read).
            let td = p.doc.tree_document(id);
            if td != DOCUMENT && p.doc.sub_gens.contains_key(&td) {
                p.doc.touch(crate::dom::StyleRecord::Text(id));
            }
            if p.doc.is_connected(id) {
                // Parsed at the next style pass (a burst of insertRule calls
                // parses once).
                if !p.pending_sheet_text.contains(&id) {
                    p.pending_sheet_text.push(id);
                }
                p.need_style = true;
                p.need_layout = true;
            }
        });
    }
    Ok(HostValue::Undefined)
});

native!(n_doc_charset, |_a| { Ok(jstr(with_page(|p| p.doc_charset).unwrap_or("UTF-8"))) });

native!(n_worker_start, |a| {
    // (url, isModule, name, shared) -> worker id (0 on failure)
    let u = st(a, 0);
    let module = boolean(a, 1, false);
    let name = st(a, 2);
    let shared = boolean(a, 3, false);
    let info = with_page(|p| (p.doc.resolve_url(&u), p.tx.clone(), p.nav_gen, p.doc.url.clone()));
    let Some((Some(url), tx, gen, doc_url)) = info else { return Ok(HostValue::from(0)) };
    // Same-origin (or blob:/data:) scripts only.
    let ok = matches!(url.scheme(), "blob" | "data") || crate::net::same_origin(&url, &doc_url) || doc_url.scheme() == "file" && url.scheme() == "file";
    if !ok {
        return Err(HostError { kind: HostErrorKind::Error, message: format!("SecurityError: Failed to construct 'Worker': Script at '{url}' cannot be accessed from origin '{}'.", doc_url.origin().ascii_serialization()) });
    }
    Ok(HostValue::from(super::worker::start(url, module, tx, gen, doc_url, name, shared)))
});

native!(n_worker_post, |a| {
    let id = num(a, 0) as u32;
    let v = a.value(1)?;
    let ports = pid_arg(a, 2);
    super::worker::post(id, v, ports);
    Ok(HostValue::Undefined)
});

native!(n_worker_terminate, |a| {
    super::worker::terminate(num(a, 0) as u32);
    Ok(HostValue::Undefined)
});

native!(n_blob_register, |a| {
    // (bytes, type) -> blob: URL
    let bytes = match a.value(0)? {
        HostValue::Bytes(b) => b,
        v => v.string().into_bytes(),
    };
    let ty = st(a, 1);
    // A worker passes its own origin.
    let origin = opt_st(a, 2).or_else(|| with_page(|p| p.doc.url.origin().ascii_serialization())).unwrap_or_else(|| "null".into());
    let url = crate::net::blob_register(&origin, bytes, &ty);
    // A document's blob: URLs go when it does (workers' stay until revoked).
    if opt_st(a, 2).is_none() {
        if let Some(id) = with_page(|p| p.tab_id) {
            crate::net::blob_set_owner(&url, id);
        }
    }
    Ok(HostValue::String(url))
});

native!(n_blob_revoke, |a| {
    crate::net::blob_revoke(&st(a, 0));
    Ok(HostValue::Undefined)
});

native!(n_tree_version, |a| {
    // Bumped by structural mutations only (child lists, not attributes/text).
    Ok(HostValue::from(with_page(|p| p.doc.tree_gen).unwrap_or(0) as f64))
});

native!(n_dom_version, |a| {
    // Bumped on every DOM mutation; live collections recompute when it changes.
    Ok(HostValue::from(with_page(|p| p.doc.generation).unwrap_or(0) as f64))
});

native!(n_elements_at, |a| {
    // (x, y, all) -> topmost element id, or all element ids under the point.
    let (x, y) = (num(a, 0) as f32, num(a, 1) as f32);
    let all = boolean(a, 2, false);
    if !x.is_finite() || !y.is_finite() {
        return Ok(if all { HostValue::Array(Vec::new()) } else { HostValue::from(-1) });
    }
    let doc = nid(a, 3).unwrap_or(DOCUMENT);
    let v = with_page(|p| {
        p.frame_ctx(doc, |p| {
            let (vw, vh) = p.viewport();
            if x < 0.0 || y < 0.0 || x >= vw || y >= vh {
                return Vec::new();
            }
            p.elements_at(x, y)
        })
    })
    .unwrap_or_default();
    Ok(if all { jids(v) } else { jid(v.first().copied()) })
});

// ------------------------------------------------------------ Web Animations

// animSet(node, id, frames [[offset, easing|null, css]...], timing [duration, delay, endDelay,
// iterations, iterationStart, direction, fill, easing], start|null, hold|null, rate, pseudo)
native!(n_anim_set, |a| {
    use crate::css::anim::*;
    let Some(node) = nid(a, 0) else { return Ok(HostValue::Undefined) };
    let id = num(a, 1) as u64;
    let frames_v = a.value(2)?;
    let timing_v = a.value(3)?;
    let start = a.number(4);
    let hold = a.number(5);
    let rate = a.number(6);
    let pseudo = st(a, 7);
    let mut frames = Vec::new();
    for f in frames_v.as_array().unwrap_or(&[]) {
        if let Some(x) = f.as_array() {
            let off = x.first().map(|v| v.number()).unwrap_or(0.0);
            let easing = x.get(1).filter(|v| !v.is_nullish()).map(|v| v.string());
            let css = x.get(2).map(|v| v.string()).unwrap_or_default();
            let comp = x.get(3).filter(|v| !v.is_nullish()).map(|v| v.string()).filter(|c| c != "auto");
            frames.push((off, easing, css, comp));
        }
    }
    let t = timing_v.as_array().unwrap_or(&[]);
    let tn = |i: usize, d: f64| t.get(i).map(|v| v.number()).filter(|v| !v.is_nan()).unwrap_or(d);
    let ts = |i: usize| t.get(i).map(|v| v.string()).unwrap_or_default();
    let direction = match ts(5).as_str() {
        "reverse" => Direction::Reverse,
        "alternate" => Direction::Alternate,
        "alternate-reverse" => Direction::AlternateReverse,
        _ => Direction::Normal,
    };
    let fill = match ts(6).as_str() {
        "forwards" => Fill::Forwards,
        "backwards" => Fill::Backwards,
        "both" => Fill::Both,
        _ => Fill::None,
    };
    let easing = parse_timing(&crate::css::tokenizer::parse_cvs(&ts(7))).unwrap_or(Timing::Linear);
    let timing = AnimTiming { duration: tn(0, 0.0), delay: tn(1, 0.0), end_delay: tn(2, 0.0), iterations: tn(3, 1.0), iteration_start: tn(4, 0.0), direction, fill, timing: easing };
    let anim = ScriptAnim {
        id,
        pseudo: match pseudo.as_str() {
            "::before" | ":before" => 1,
            "::after" | ":after" => 2,
            _ => 0,
        },
        keyframes: std::rc::Rc::new(keyframes_from_script(&frames, Some(ts(8).as_str()).filter(|c| !c.is_empty()))),
        timing,
        start: if start.is_finite() { Some(start) } else { None },
        hold: if hold.is_finite() { Some(hold) } else { None },
        rate: if rate.is_finite() { rate } else { 1.0 },
    };
    with_page(|p| {
        let key = match anim.pseudo {
            1 => node | crate::css::ANIM_BEFORE,
            2 => node | crate::css::ANIM_AFTER,
            _ => node,
        };
        for list in p.anim.script.values_mut() {
            list.retain(|x| x.id != id);
        }
        p.anim.script.retain(|_, l| !l.is_empty());
        p.anim.script.entry(key).or_default().push(ScriptAnim { pseudo: 0, ..anim });
        p.need_style = true;
        { p.full_style = true; crate::page::full_why(file!(), line!()); }
    });
    Ok(HostValue::Undefined)
});

native!(n_anim_remove, |a| {
    let id = num(a, 0) as u64;
    with_page(|p| {
        for list in p.anim.script.values_mut() {
            list.retain(|x| x.id != id);
        }
        p.anim.script.retain(|_, l| !l.is_empty());
        p.need_style = true;
        { p.full_style = true; crate::page::full_why(file!(), line!()); }
    });
    Ok(HostValue::Undefined)
});

/// Every native of a page or worker scope: the DOM/BOM natives plus the
/// locale services.
pub fn all() -> Vec<(&'static str, NativeFn, u32)> {
    let mut v = NATIVES.to_vec();
    v.extend_from_slice(super::intl::INTL_NATIVES);
    v.extend_from_slice(super::media::MEDIA_NATIVES);
    v
}

/// The natives, as (name, function, arity).
pub static NATIVES: &[(&str, NativeFn, u32)] = &[
    ("elementsAt", n_elements_at, 4),
    ("animSet", n_anim_set, 8),
    ("animRemove", n_anim_remove, 1),
    ("domVersion", n_dom_version, 0),
    ("workerStart", n_worker_start, 2),
    ("workerPostTo", n_worker_post, 3),
    ("workerTerminate", n_worker_terminate, 1),
    ("blobRegister", n_blob_register, 3),
    ("blobRevoke", n_blob_revoke, 1),
    ("fetchBody", n_fetch_body, 1),
    ("fetchDrop", n_fetch_drop, 1),
    ("utf8Encode", n_utf8_encode, 1),
    ("textDecode", n_text_decode, 4),
    ("encodingName", n_encoding_name, 1),
    ("textDecoderNew", n_text_decoder_new, 2),
    ("textDecodeChunk", n_text_decode_chunk, 4),
    ("docCharset", n_doc_charset, 0),
    ("selectorValid", n_selector_valid, 2),
    ("urlSet", n_url_set, 3),
    ("baseURL", n_base_url, 0),
    ("formDecode", n_form_decode, 1),
    ("formEncode", n_form_encode, 1),
    ("type", n_type, 1),
    ("templateContent", n_template_content, 1),
    ("name", n_name, 1),
    ("ns", n_ns, 1),
    ("parent", n_parent, 1),
    ("children", n_children, 1),
    ("elchildren", n_elchildren, 1),
    ("next", n_next, 1),
    ("prev", n_prev, 1),
    ("nextEl", n_next_el, 1),
    ("prevEl", n_prev_el, 1),
    ("text", n_text, 1),
    ("setText", n_set_text, 2),
    ("innerText", n_inner_text, 1),
    ("attr", n_attr, 2),
    ("setAttr", n_set_attr, 3),
    ("removeAttr", n_remove_attr, 2),
    ("attrs", n_attrs, 1),
    ("create", n_create, 3),
    ("createDocument", n_create_document, 1),
    ("adopt", n_adopt, 2),
    ("createDoctype", n_create_doctype, 2),
    ("doctypeIds", n_doctype_ids, 1),
    ("parseInto", n_parse_into, 3),
    ("parseXMLInto", n_parse_xml_into, 2),
    ("docIsXml", n_doc_is_xml, 0),
    ("docContentType", n_doc_content_type, 0),
    ("owner", n_owner, 1),
    ("root", n_root, 1),
    ("createText", n_create_text, 1),
    ("createComment", n_create_comment, 1),
    ("createCDATA", n_create_cdata, 2),
    ("createPI", n_create_pi, 3),
    ("attrNss", n_attr_nss, 1),
    ("domGen", n_dom_gen, 0),
    ("topLayer", n_top_layer, 2),
    ("setPopoverOpen", n_set_popover_open, 2),
    ("attrIndexNS", n_attr_index_ns, 3),
    ("setAttrNS", n_set_attr_ns, 4),
    ("removeAttrNS", n_remove_attr_ns, 3),
    ("piTarget", n_pi_target, 1),
    ("createFragment", n_create_fragment, 0),
    ("append", n_append, 2),
    ("insertBefore", n_insert_before, 3),
    ("remove", n_remove, 1),
    ("clone", n_clone, 2),
    ("innerHTML", n_inner_html, 1),
    ("getHTML", n_get_html, 3),
    ("attachDeclarative", n_attach_declarative, 1),
    ("outerHTML", n_outer_html, 1),
    ("setInnerHTML", n_set_inner_html, 2),
    ("parseFragment", n_parse_fragment, 3),
    ("query", n_query, 3),
    ("matches", n_matches, 2),
    ("closest", n_closest, 2),
    ("byId", n_by_id, 1),
    ("byTag", n_by_tag, 3),
    ("namedProps", n_named_props, 2),
    ("byClass", n_by_class, 2),
    ("contains", n_contains, 2),
    ("connected", n_connected, 1),
    ("attachShadow", n_attach_shadow, 6),
    ("shadowRoot", n_shadow_root, 1),
    ("shadowInfo", n_shadow_info, 1),
    ("assignedNodes", n_assigned_nodes, 1),
    ("assignedSlot", n_assigned_slot, 1),
    ("slotAssign", n_slot_assign, 2),
    ("setAdopted", n_set_adopted, 2),
    ("ceWalk", n_ce_walk, 1),
    ("composedRoot", n_composed_root, 1),
    ("scriptStarted", n_script_started, 1),
    ("scriptKnown", n_script_known, 1),
    ("scriptMoved", n_script_moved, 1),
    ("crossOriginIsolated", n_cross_origin_isolated, 0),
    ("coiSW", n_coi_sw, 3),
    ("ceDefined", n_ce_defined, 1),
    ("shadowCount", n_shadow_count, 0),
    ("sContains", n_s_contains, 2),
    ("slotSnapshot", n_slot_snapshot, 0),
    ("value", n_value, 1),
    ("setValue", n_set_value, 2),
    ("checked", n_checked, 1),
    ("formState", n_form_state, 3),
    ("cssColor", n_css_color, 1),
    ("setChecked", n_set_checked, 2),
    ("selected", n_selected, 1),
    ("setSelected", n_set_selected, 2),
    ("focus", n_focus, 1),
    ("focused", n_focused, 0),
    ("wsOpen", n_ws_open, 3),
    ("wsSend", n_ws_send, 3),
    ("wsClose", n_ws_close, 3),
    ("formCharsetEncode", n_form_charset_encode, 2),
    ("focusRendered", n_focus_rendered, 1),
    ("active", n_active, 0),
    ("caret", n_caret, 1),
    ("rect", n_rect, 1),
    ("clientRects", n_client_rects, 1),
    ("boxInfo", n_box_info, 1),
    ("setScroll", n_set_scroll, 3),
    ("computed", n_computed, 3),
    ("computedExists", n_computed_exists, 2),
    ("connectedAny", n_connected_any, 1),
    ("firstChild", n_first_child, 1),
    ("treeVersion", n_tree_version, 0),
    ("lastChild", n_last_child, 1),
    ("viewport", n_viewport, 0),
    ("scrollTo", n_scroll_to, 2),
    ("url", n_url, 0),
    ("navigate", n_navigate, 2),
    ("reload", n_reload, 0),
    ("historyGo", n_history_go, 1),
    ("pushState", n_push_state, 3),
    ("historyState", n_history_state, 0),
    ("title", n_title, 0),
    ("setTitle", n_set_title, 1),
    ("cookie", n_cookie, 0),
    ("setCookie", n_set_cookie, 1),
    ("storage", n_storage, 4),
    ("idbLoad", n_idb_load, 1),
    ("idbSave", n_idb_save, 2),
    ("idbDelete", n_idb_delete, 1),
    ("idbNames", n_idb_names, 0),
    ("alert", n_alert, 1),
    ("permissionRequest", n_permission_request, 2),
    ("permissionState", n_permission_state, 1),
    ("clipboardWrite", n_clipboard_write, 1),
    ("pasteText", n_paste_text, 1),
    ("log", n_log, 4),
    ("dtAttached", n_dt_attached, 0),
    ("frameRemote", n_frame_remote, 1),
    ("frameLiveOk", n_frame_live_ok, 2),
    ("frameLocalOk", n_frame_local_ok, 2),
    ("frameDoc", n_frame_doc, 3),
    ("frameViewport", n_frame_viewport, 1),
    ("frameLoaded", n_frame_loaded, 1),
    ("framesPending", n_frames_pending, 1),
    ("frameNavigate", n_frame_navigate, 3),
    ("framePost", n_frame_post, 4),
    ("parentPost", n_parent_post, 4),
    ("portPair", n_port_pair, 0),
    ("portPost", n_port_post, 3),
    ("textRangeRects", n_text_range_rects, 3),
    ("caretFromPoint", n_caret_from_point, 3),
    ("portClose", n_port_close, 1),
    ("frameInfo", n_frame_info, 0),
    ("navigateTop", n_navigate_top, 1),
    ("wptEnabled", n_wpt_enabled, 0),
    ("noteWheel", n_note_wheel, 0),
    ("fontFaces", n_font_faces, 0),
    ("fontLoad", n_font_load, 4),
    ("fontsPending", n_fonts_pending, 0),
    ("fontFaceAdded", n_font_face_added, 1),
    ("addFontBytes", n_add_font_bytes, 6),
    ("linkSheetText", n_link_sheet_text, 1),
    ("importSheetText", n_import_sheet_text, 1),
    ("linkSheetGen", n_link_sheet_gen, 1),
    ("setLinkSheetText", n_set_link_sheet_text, 2),
    ("scrollIntoView", n_scroll_into_view, 3),
    ("wptInput", n_wpt_input, 6),
    ("wptSync", n_wpt_sync, 1),
    ("dtSend", n_dt_send, 2),
    ("dtClose", n_dt_close, 0),
    ("dtTarget", n_dt_target, 0),
    ("now", n_now, 0),
    ("timelineNow", n_timeline_now, 0),
    ("easeEval", n_ease_eval, 3),
    ("cssAnimatable", n_css_animatable, 1),
    ("easingCanon", n_easing_canon, 1),
    ("cssNumber", n_css_number, 1),
    ("cssLonghands", n_css_longhands, 1),
    ("cssAnims", n_css_anims, 0),
    ("cssAnimSeek", n_css_anim_seek, 2),
    ("cssAnimCancel", n_css_anim_cancel, 1),
    ("cssAnimPlay", n_css_anim_play, 2),
    ("cssLonghandNames", n_css_longhand_names, 0),
    ("mediaCanon", n_media_canon, 1),
    ("supportsValid", n_supports_valid, 1),
    ("setStyleText", n_set_style_text, 2),
    ("styleOverridden", n_style_overridden, 1),
    ("randomBytes", n_random_bytes, 1),
    ("resolveURL", n_resolve_url, 3),
    ("urlParts", n_url_parts, 2),
    ("openTab", n_open_tab, 1),
    ("pointerLock", n_pointer_lock, 1),
    ("media", n_media, 1),
    ("cssSupports", n_css_supports, 1),
    ("matchesSyntax", n_matches_syntax, 2),
    ("cssKnown", n_css_known, 1),
    ("cssColorSpec", n_css_color_spec, 1),
    ("cssBlock", n_css_block, 5),
    ("cssDeclOk", n_css_decl_ok, 2),
    ("traceMissing", n_trace_missing, 0),
    ("traceSlow", n_trace_slow, 0),
    ("zip", n_zip, 3),
    ("boxQuad", n_box_quad, 2),
    ("roSizes", n_ro_sizes, 1),
    ("layoutRect", n_layout_rect, 1),
    ("zipOpen", n_zip_open, 2),
    ("zipPush", n_zip_push, 2),
    ("zipEnd", n_zip_end, 1),
    ("zipClose", n_zip_close, 1),
    ("fetch", n_fetch, 12),
    ("fetchSync", n_fetch_sync, 11),
    ("decodeDoc", n_decode_doc, 3),
    ("selectionChanged", n_selection_changed, 4),
    ("setDesignMode", n_set_design_mode, 1),
    ("editingSource", n_editing_source, 0),
    ("digest", n_digest, 2),
    ("webcrypto", n_webcrypto, 2),
    ("scriptForceAsync", n_script_force_async, 2),
    ("fetchRead", n_fetch_read, 1),
    ("fetchWant", n_fetch_want, 1),
    ("fetchAbort", n_fetch_abort, 1),
    ("scriptInserted", n_script_inserted, 1),
    ("imgInfo", n_img_info, 1),
    ("imgSelect", n_img_select, 4),
    ("formSubmit", n_form_submit, 1),
    ("requestSubmit", n_request_submit, 2),
    ("clickDefault", n_click_default, 1),
    ("canvas", n_canvas, 8),
    ("quirks", n_quirks, 0),
    ("defineElement", n_define_element, 1),
    ("preActivate", n_pre_activate, 1),
    ("finishActivation", n_finish_activation, 3),
    ("docWrite", n_doc_write, 2),
    ("readyState", n_ready_state, 0),
    ("btoa", n_btoa, 1),
    ("atob", n_atob, 1),
];
