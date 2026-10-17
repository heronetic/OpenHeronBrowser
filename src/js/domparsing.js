// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// HeronScript DOM Parsing: innerHTML/outerHTML for elements of XML
// documents. Evaluated after the prelude, over the public DOM.
//
// The fragment parser is a small non-validating XML 1.0 + Namespaces parser:
// elements, attributes, namespaces, text, entity and character references,
// CDATA, comments and processing instructions.
(function (global) {
  'use strict';
  if (typeof global.Document !== 'function') return;
  const hidden = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: true, configurable: true });
  const HTML_NS = 'http://www.w3.org/1999/xhtml';
  const XML_NS = 'http://www.w3.org/XML/1998/namespace';
  const XMLNS_NS = 'http://www.w3.org/2000/xmlns/';
  const NAME_START = 'A-Z_a-z:\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD\\uD800-\\uDBFF\\uDC00-\\uDFFF';
  const NAME_CHARS = NAME_START + '\\-.0-9\\u00B7\\u0300-\\u036F\\u203F-\\u2040';
  const NAME_AT = new RegExp('[' + NAME_START + '][' + NAME_CHARS + ']*', 'y');
  const WS = /[ \t\r\n]+/y;
  const isHTMLDoc = (d) => !!d && d.nodeType === 9 && (d._html === true && !(d instanceof global.XMLDocument) || d._isHTML !== false && !(d instanceof global.XMLDocument) && (d.contentType === undefined || d.contentType === 'text/html'));

  // ------------------------------------------------------------ XML parser
  class XMLError extends Error {}
  function isXMLChar(c) {
    return c === 0x9 || c === 0xA || c === 0xD || (c >= 0x20 && c <= 0xD7FF) || (c >= 0xE000 && c <= 0xFFFD) || (c >= 0x10000 && c <= 0x10FFFF);
  }
  const PREDEF = { lt: '<', gt: '>', amp: '&', quot: '"', apos: "'" };

  /**
   * Parse `src` into `doc` (an empty document), or into `parent` (fragment
   * mode, with `nsScope` as the in-scope namespaces). Throws XMLError with
   * the partial tree left in place.
   */
  function parseXMLInto(src, doc, parent, nsScope) {
    let i = 0;
    const n = src.length;
    const entities = Object.create(null);
    const fragment = !!parent;
    const stack = []; // { el, qname, ns: Map }
    let root = null;
    let seenDoctype = false;
    const where = () => {
      let line = 1, col = 1;
      for (let k = 0; k < i && k < n; k++) { if (src.charCodeAt(k) === 10) { line++; col = 1; } else col++; }
      return `line ${line} at column ${col}`;
    };
    const fail = (msg) => { throw new XMLError(`error on ${where()}: ${msg}`); };
    const startsAt = (s) => src.startsWith(s, i);
    const skipWS = () => { WS.lastIndex = i; if (WS.test(src)) i = WS.lastIndex; };
    const name = () => {
      NAME_AT.lastIndex = i;
      const m = NAME_AT.exec(src);
      if (!m) fail('Name expected');
      i = NAME_AT.lastIndex;
      return m[0];
    };
    const scope = () => (stack.length ? stack[stack.length - 1].ns : nsScope);
    const current = () => (stack.length ? stack[stack.length - 1].el : (parent || doc));
    function decodeRefs(s, inAttr, depth = 0) {
      if (s.indexOf('&') < 0) return s;
      let out = '';
      let k = 0;
      while (k < s.length) {
        const amp = s.indexOf('&', k);
        if (amp < 0) { out += s.slice(k); break; }
        out += s.slice(k, amp);
        const semi = s.indexOf(';', amp);
        if (semi < 0) fail("EntityRef: expecting ';'");
        const ref = s.slice(amp + 1, semi);
        if (ref[0] === '#') {
          const code = ref[1] === 'x' ? (/^x[0-9a-fA-F]+$/.test(ref.slice(1)) ? parseInt(ref.slice(2), 16) : NaN) : (/^#[0-9]+$/.test('#' + ref.slice(1)) ? parseInt(ref.slice(1), 10) : NaN);
          if (!(code >= 0) || !isXMLChar(code)) fail(`xmlParseCharRef: invalid xmlChar value ${code}`);
          out += String.fromCodePoint(code);
        } else if (PREDEF[ref] !== undefined) out += PREDEF[ref];
        else if (entities[ref] !== undefined) {
          if (depth > 8) fail(`Detected an entity reference loop`);
          out += decodeRefs(entities[ref], inAttr, depth + 1);
        } else fail(`Entity '${ref}' not defined`);
        k = semi + 1;
      }
      return out;
    }
    function quoted() {
      const q = src[i];
      if (q !== '"' && q !== "'") fail('String not started expecting \' or "');
      const end = src.indexOf(q, i + 1);
      if (end < 0) fail('String not closed');
      const v = src.slice(i + 1, end);
      i = end + 1;
      return v;
    }
    function append(node) { current().appendChild(node); }
    function checkChars(s) {
      for (let k = 0; k < s.length; k++) {
        const c = s.codePointAt(k);
        if (c > 0xFFFF) k++;
        if (!isXMLChar(c)) fail(`PCDATA invalid Char value ${c}`);
      }
    }

    // XML declaration
    if (!fragment) {
      if (src.charCodeAt(0) === 0xFEFF) i = 1;
      if (startsAt('<?xml') && /[ \t\r\n?]/.test(src[i + 5] || '')) {
        const end = src.indexOf('?>', i);
        if (end < 0) fail('parsing XML declaration: \'?>\' expected');
        const decl = src.slice(i + 5, end);
        if (!/^\s+version\s*=\s*(['"])1\.[0-9]+\1/.test(decl)) fail('Malformed declaration expecting version');
        i = end + 2;
      }
    }
    while (i < n) {
      const c = src[i];
      if (c === '<') {
        if (startsAt('<!--')) {
          const end = src.indexOf('-->', i + 4);
          if (end < 0) fail('Comment not terminated');
          const data = src.slice(i + 4, end);
          if (data.indexOf('--') >= 0 || data.endsWith('-')) fail('Double hyphen within comment');
          checkChars(data);
          append(doc.createComment(data));
          i = end + 3;
          continue;
        }
        if (startsAt('<![CDATA[')) {
          if (!stack.length && !fragment) fail('StartTag: invalid element name');
          const end = src.indexOf(']]>', i + 9);
          if (end < 0) fail('CData section not finished');
          const data = src.slice(i + 9, end);
          checkChars(data);
          append(doc.createCDATASection(data));
          i = end + 3;
          continue;
        }
        if (startsAt('<!DOCTYPE')) {
          if (fragment || seenDoctype || root) fail('StartTag: invalid element name');
          seenDoctype = true;
          i += 9;
          skipWS();
          const dtName = name();
          skipWS();
          let pub = '', sys = '';
          if (startsAt('PUBLIC')) { i += 6; skipWS(); pub = quoted(); skipWS(); if (src[i] === '"' || src[i] === "'") { sys = quoted(); skipWS(); } }
          else if (startsAt('SYSTEM')) { i += 6; skipWS(); sys = quoted(); skipWS(); }
          if (src[i] === '[') {
            // Internal subset: keep general entity declarations, skip the rest.
            i++;
            for (;;) {
              skipWS();
              if (i >= n) fail('DOCTYPE improperly terminated');
              if (src[i] === ']') { i++; break; }
              if (startsAt('<!ENTITY')) {
                i += 8; skipWS();
                const pe = src[i] === '%';
                if (pe) { i++; skipWS(); }
                const en = name(); skipWS();
                if (src[i] === '"' || src[i] === "'") { const v = quoted(); if (!pe && entities[en] === undefined) entities[en] = v; }
                const end = src.indexOf('>', i);
                if (end < 0) fail('EntityValue: " or \' expected');
                i = end + 1;
              } else if (startsAt('<!--')) {
                const end = src.indexOf('-->', i + 4);
                if (end < 0) fail('Comment not terminated');
                i = end + 3;
              } else if (startsAt('<?')) {
                const end = src.indexOf('?>', i);
                if (end < 0) fail('PI not terminated');
                i = end + 2;
              } else if (startsAt('<!')) {
                // ELEMENT / ATTLIST / NOTATION: skip to the closing '>' outside quotes.
                let q = null;
                i += 2;
                while (i < n) { const ch = src[i]; if (q) { if (ch === q) q = null; } else if (ch === '"' || ch === "'") q = ch; else if (ch === '>') break; i++; }
                i++;
              } else if (src[i] === '%') {
                const end = src.indexOf(';', i);
                i = end < 0 ? n : end + 1;
              } else fail('internal error: DOCTYPE');
            }
            skipWS();
          }
          if (src[i] !== '>') fail('DOCTYPE improperly terminated');
          i++;
          const dt = doc.implementation.createDocumentType(dtName, pub, sys);
          doc.appendChild(dt);
          continue;
        }
        if (startsAt('<?')) {
          i += 2;
          const target = name();
          if (target.toLowerCase() === 'xml') fail('XML declaration allowed only at the start of the document');
          let data = '';
          const end = src.indexOf('?>', i);
          if (end < 0) fail('PI not terminated');
          if (end > i) {
            if (!/[ \t\r\n]/.test(src[i])) fail('ParsePI: PI ' + target + ' space expected');
            data = src.slice(i, end).replace(/^[ \t\r\n]+/, '');
          }
          checkChars(data);
          i = end + 2;
          append(doc.createProcessingInstruction(target, data));
          continue;
        }
        if (startsAt('</')) {
          i += 2;
          const qn = name();
          skipWS();
          if (src[i] !== '>') fail('expected \'>\'');
          i++;
          const top = stack[stack.length - 1];
          if (!top) fail(`Unexpected end tag : ${qn}`);
          if (top.qname !== qn) fail(`Opening and ending tag mismatch: ${top.qname} and ${qn}`);
          stack.pop();
          continue;
        }
        // Start tag.
        i++;
        if (!fragment && root && !stack.length) fail('Extra content at the end of the document');
        const qn = name();
        const attrs = [];
        let selfClose = false;
        for (;;) {
          const before = i;
          skipWS();
          if (startsAt('/>')) { i += 2; selfClose = true; break; }
          if (src[i] === '>') { i++; break; }
          if (i >= n) fail(`Couldn't find end of Start Tag ${qn}`);
          if (i === before) fail('attributes construct error');
          const an = name();
          skipWS();
          if (src[i] !== '=') fail(`Specification mandates value for attribute ${an}`);
          i++;
          skipWS();
          const raw = quoted();
          if (raw.indexOf('<') >= 0) fail(`Unescaped '<' not allowed in attributes values`);
          checkChars(raw);
          const val = decodeRefs(raw.replace(/[\t\n\r]/g, ' '), true);
          if (attrs.some((a) => a.qn === an)) fail(`Attribute ${an} redefined`);
          attrs.push({ qn: an, v: val });
        }
        // Namespace declarations first.
        let ns = scope();
        let copied = false;
        for (const a of attrs) {
          if (a.qn === 'xmlns' || a.qn.startsWith('xmlns:')) {
            if (!copied) { ns = new Map(ns); copied = true; }
            const p = a.qn === 'xmlns' ? '' : a.qn.slice(6);
            if (p === 'xmlns') fail('xmlns: prefix xmlns reserved');
            if (p && !a.v) fail(`xmlns:${p}: Empty XML namespace is not allowed`);
            if (p === 'xml' && a.v !== XML_NS) fail('xml namespace prefix mapped to wrong URI');
            ns.set(p, a.v || null);
          }
        }
        const colon = qn.indexOf(':');
        const prefix = colon >= 0 ? qn.slice(0, colon) : '';
        let uri = prefix === 'xml' ? XML_NS : ns.get(prefix);
        if (uri === undefined) {
          if (prefix) fail(`Namespace prefix ${prefix} on ${qn.slice(colon + 1)} is not defined`);
          uri = null;
        }
        let el;
        try { el = doc.createElementNS(uri || null, qn); } catch (e) { fail(`StartTag: invalid element name`); }
        const seen = new Set();
        for (const a of attrs) {
          let anUri = null;
          if (a.qn === 'xmlns' || a.qn.startsWith('xmlns:')) anUri = XMLNS_NS;
          else {
            const ac = a.qn.indexOf(':');
            if (ac >= 0) {
              const ap = a.qn.slice(0, ac);
              anUri = ap === 'xml' ? XML_NS : ns.get(ap);
              if (!anUri) fail(`Namespace prefix ${ap} for ${a.qn.slice(ac + 1)} on ${qn} is not defined`);
            }
          }
          const key = (anUri || '') + '|' + a.qn.slice(a.qn.indexOf(':') + 1);
          if (anUri && seen.has(key)) fail(`Namespaced Attribute ${a.qn.slice(a.qn.indexOf(':') + 1)} in '${anUri}' redefined`);
          seen.add(key);
          try { el.setAttributeNS(anUri, a.qn, a.v); } catch (e) { fail(`attributes construct error`); }
        }
        append(el);
        if (!stack.length && !fragment) root = el;
        if (!selfClose) stack.push({ el, qname: qn, ns });
        continue;
      }
      // Character data.
      let end = src.indexOf('<', i);
      if (end < 0) end = n;
      const raw = src.slice(i, end);
      if (!stack.length && !fragment) {
        if (/[^ \t\r\n]/.test(raw)) fail(root ? 'Extra content at the end of the document' : "Start tag expected, '<' not found");
        i = end;
        continue;
      }
      if (raw.indexOf(']]>') >= 0) fail("Sequence ']]>' not allowed in content");
      checkChars(raw);
      const text = decodeRefs(raw.replace(/\r\n?/g, '\n'), false);
      i = end;
      if (text) {
        const cur = current();
        const last = cur.lastChild;
        if (last && last.nodeType === 3) last.appendData(text);
        else cur.appendChild(doc.createTextNode(text));
      }
    }
    if (stack.length) fail(`Premature end of data in tag ${stack[stack.length - 1].qname}`);
    if (!fragment && !root) fail('Document is empty');
  }

  // DOMParser and XMLSerializer live in the prelude (the XML parser for
  // documents is native); this file keeps the fragment parser used by
  // innerHTML/outerHTML in XML documents.
  const xmlSerialize = (node, childrenOnly) => global.__ohb_xmlSerialize(node, true, childrenOnly);

  // ------------------------------- innerHTML / outerHTML in XML documents
  function ownerDoc(n) { return n.nodeType === 9 ? n : n.ownerDocument; }
  function inScopeNamespaces(el) {
    const m = new Map([['', null]]);
    const chain = [];
    for (let e = el; e && e.nodeType === 1; e = e.parentNode) chain.unshift(e);
    for (const e of chain) {
      if (e.namespaceURI !== null && e.prefix === null) m.set('', e.namespaceURI);
      else if (e.prefix !== null) m.set(e.prefix, e.namespaceURI);
      for (const a of Array.from(e.attributes)) {
        if (a.namespaceURI !== XMLNS_NS) continue;
        m.set(a.prefix === null ? '' : a.localName, a.value || null);
      }
    }
    if (el && el.nodeType === 1 && el.prefix === null) m.set('', el.namespaceURI);
    return m;
  }
  function parseXMLFragment(markup, contextEl) {
    const doc = ownerDoc(contextEl);
    const frag = doc.createDocumentFragment();
    try {
      parseXMLInto(String(markup), doc, frag, inScopeNamespaces(contextEl));
    } catch (e) {
      if (!(e instanceof XMLError)) throw e;
      throw new DOMException("Failed to set the 'innerHTML' property on 'Element': The provided markup is invalid XML, and therefore cannot be inserted into an XML document.", 'SyntaxError');
    }
    return frag;
  }
  const EP = global.Element.prototype;
  const inner = Object.getOwnPropertyDescriptor(EP, 'innerHTML');
  const outer = Object.getOwnPropertyDescriptor(EP, 'outerHTML');
  if (inner && inner.get) {
    Object.defineProperty(EP, 'innerHTML', {
      get() { return isHTMLDoc(ownerDoc(this)) ? inner.get.call(this) : xmlSerialize(this, true); },
      set(v) {
        if (isHTMLDoc(ownerDoc(this))) return inner.set.call(this, v);
        const frag = parseXMLFragment(v === null ? '' : v, this);
        const target = this.localName === 'template' && this.namespaceURI === HTML_NS ? this.content : this;
        while (target.firstChild) target.removeChild(target.firstChild);
        target.appendChild(frag);
      },
      configurable: true, enumerable: true,
    });
  }
  if (outer && outer.get) {
    Object.defineProperty(EP, 'outerHTML', {
      get() { return isHTMLDoc(ownerDoc(this)) ? outer.get.call(this) : xmlSerialize(this, false); },
      set(v) {
        if (isHTMLDoc(ownerDoc(this))) return outer.set.call(this, v);
        const p = this.parentNode;
        if (!p) return;
        if (p.nodeType === 9) throw new DOMException("Failed to set the 'outerHTML' property on 'Element': This element's parent is of type '#document', which is not an element node.", 'NoModificationAllowedError');
        const ctx = p.nodeType === 11 ? ownerDoc(this).createElementNS(HTML_NS, 'body') : p;
        const frag = parseXMLFragment(v === null ? '' : v, ctx);
        p.replaceChild(frag, this);
      },
      configurable: true, enumerable: true,
    });
  }

})(globalThis);
