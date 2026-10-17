// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// HeronScript XPath 1.0: document.evaluate, XPathEvaluator, XPathExpression
// and XPathResult over the public DOM. Evaluated after the prelude.
//
// The engine is a plain tokenizer -> recursive-descent parser -> tree-walking
// evaluator. Node-sets are arrays kept in document order without duplicates.
// HTML documents follow the HTML-specific rules: an unprefixed name test
// matches HTML elements by lowercased local name.
(function (global) {
  'use strict';
  if (typeof global.Document !== 'function') return;
  const hidden = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: true, configurable: true });
  const HTML_NS = 'http://www.w3.org/1999/xhtml';
  const XML_NS = 'http://www.w3.org/XML/1998/namespace';
  const syntaxError = (msg) => new DOMException("Failed to execute 'evaluate' on 'Document': " + msg, 'SyntaxError');
  const typeError = (msg) => new TypeError(msg);
  const asciiLower = (x) => x.replace(/[A-Z]+/g, (c) => c.toLowerCase());
  const N = global.__n;
  const domGen = () => (N && N.domGen ? N.domGen() : 0);

  // ------------------------------------------------------------- tokenizer
  const NAME_START = /[A-Za-z_À-ÖØ-öø-˿Ͱ-ͽͿ-῿‌-‍⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�]/;
  const NAME_CHAR = /[A-Za-z0-9_.\-·À-ÖØ-öø-ͽͿ-῿‌-‍‿⁀⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�]/;
  const NODE_TYPES = new Set(['comment', 'text', 'processing-instruction', 'node']);
  const AXES = new Set(['ancestor', 'ancestor-or-self', 'attribute', 'child', 'descendant', 'descendant-or-self',
    'following', 'following-sibling', 'namespace', 'parent', 'preceding', 'preceding-sibling', 'self']);

  function tokenize(src) {
    const out = [];
    let i = 0;
    const n = src.length;
    while (i < n) {
      const c = src[i];
      if (c === ' ' || c === '\t' || c === '\n' || c === '\r') { i++; continue; }
      if (c === '"' || c === "'") {
        const j = src.indexOf(c, i + 1);
        if (j < 0) throw syntaxError(`The string '${src}' is not a valid XPath expression.`);
        out.push({ t: 'str', v: src.slice(i + 1, j) });
        i = j + 1;
        continue;
      }
      if ((c >= '0' && c <= '9') || (c === '.' && src[i + 1] >= '0' && src[i + 1] <= '9')) {
        let j = i;
        while (j < n && src[j] >= '0' && src[j] <= '9') j++;
        if (src[j] === '.') { j++; while (j < n && src[j] >= '0' && src[j] <= '9') j++; }
        out.push({ t: 'num', v: parseFloat(src.slice(i, j)) });
        i = j;
        continue;
      }
      const two = src.substr(i, 2);
      if (two === '..' || two === '::' || two === '//' || two === '!=' || two === '<=' || two === '>=') {
        out.push({ t: 'op', v: two });
        i += 2;
        continue;
      }
      if ('()[].@,|+-=<>/*$'.indexOf(c) >= 0) { out.push({ t: 'op', v: c }); i++; continue; }
      if (NAME_START.test(c)) {
        let j = i + 1;
        while (j < n && NAME_CHAR.test(src[j])) j++;
        let name = src.slice(i, j);
        // QName or prefix:*
        if (src[j] === ':' && src[j + 1] !== ':') {
          if (src[j + 1] === '*') { name += ':*'; j += 2; }
          else if (j + 1 < n && NAME_START.test(src[j + 1])) {
            let k = j + 2;
            while (k < n && NAME_CHAR.test(src[k])) k++;
            name = src.slice(i, k);
            j = k;
          }
        }
        out.push({ t: 'name', v: name });
        i = j;
        continue;
      }
      throw syntaxError(`The string '${src}' is not a valid XPath expression.`);
    }
    // Disambiguation (XPath 1.0 section 3.7).
    for (let k = 0; k < out.length; k++) {
      const tok = out[k];
      const prev = out[k - 1];
      const prevIsOperand = prev && !(prev.t === 'op' && ['@', '::', '(', '[', ',', '/', '//', '|', '+', '-', '=', '!=', '<', '<=', '>', '>=', '$'].includes(prev.v))
        && !(prev.t === 'operator') && !(prev.t === 'mul');
      if (tok.t === 'op' && tok.v === '*' && prevIsOperand) { tok.t = 'mul'; continue; }
      if (tok.t === 'name') {
        if (prevIsOperand && (tok.v === 'and' || tok.v === 'or' || tok.v === 'mod' || tok.v === 'div')) { tok.t = 'operator'; continue; }
        const next = out[k + 1];
        if (next && next.t === 'op' && next.v === '(') tok.t = NODE_TYPES.has(tok.v) ? 'nodetype' : 'func';
        else if (next && next.t === 'op' && next.v === '::') tok.t = 'axis';
      }
    }
    return out;
  }

  // ---------------------------------------------------------------- parser
  function parse(src) {
    const toks = tokenize(src);
    let p = 0;
    const peek = (o = 0) => toks[p + o];
    const isOp = (v, o = 0) => { const t = toks[p + o]; return !!t && t.t === 'op' && t.v === v; };
    const fail = () => { throw syntaxError(`The string '${src}' is not a valid XPath expression.`); };
    const expect = (v) => { if (!isOp(v)) fail(); p++; };

    function orExpr() {
      let l = andExpr();
      while (peek() && peek().t === 'operator' && peek().v === 'or') { p++; l = { k: 'or', l, r: andExpr() }; }
      return l;
    }
    function andExpr() {
      let l = eqExpr();
      while (peek() && peek().t === 'operator' && peek().v === 'and') { p++; l = { k: 'and', l, r: eqExpr() }; }
      return l;
    }
    function eqExpr() {
      let l = relExpr();
      while (isOp('=') || isOp('!=')) { const op = toks[p++].v; l = { k: 'cmp', op, l, r: relExpr() }; }
      return l;
    }
    function relExpr() {
      let l = addExpr();
      while (isOp('<') || isOp('>') || isOp('<=') || isOp('>=')) { const op = toks[p++].v; l = { k: 'cmp', op, l, r: addExpr() }; }
      return l;
    }
    function addExpr() {
      let l = mulExpr();
      while (isOp('+') || isOp('-')) { const op = toks[p++].v; l = { k: 'arith', op, l, r: mulExpr() }; }
      return l;
    }
    function mulExpr() {
      let l = unaryExpr();
      for (;;) {
        const t = peek();
        if (t && t.t === 'mul') { p++; l = { k: 'arith', op: '*', l, r: unaryExpr() }; }
        else if (t && t.t === 'operator' && (t.v === 'div' || t.v === 'mod')) { p++; l = { k: 'arith', op: t.v, l, r: unaryExpr() }; }
        else return l;
      }
    }
    function unaryExpr() {
      if (isOp('-')) { p++; return { k: 'neg', e: unaryExpr() }; }
      return unionExpr();
    }
    function unionExpr() {
      let l = pathExpr();
      while (isOp('|')) { p++; l = { k: 'union', l, r: pathExpr() }; }
      return l;
    }
    function startsFilter() {
      const t = peek();
      if (!t) return false;
      if (t.t === 'str' || t.t === 'num' || t.t === 'func') return true;
      return t.t === 'op' && (t.v === '(' || t.v === '$');
    }
    function pathExpr() {
      if (startsFilter()) {
        const prim = primary();
        const preds = [];
        while (isOp('[')) preds.push(predicate());
        let e = preds.length ? { k: 'filter', e: prim, preds } : prim;
        if (isOp('/') || isOp('//')) {
          const steps = [];
          relPath(steps);
          e = { k: 'path', abs: false, from: e, steps };
        }
        return e;
      }
      return locationPath();
    }
    function primary() {
      const t = toks[p++];
      if (t.t === 'str') return { k: 'lit', v: t.v };
      if (t.t === 'num') return { k: 'lit', v: t.v };
      if (t.t === 'op' && t.v === '(') { const e = orExpr(); expect(')'); return e; }
      if (t.t === 'op' && t.v === '$') {
        const n = toks[p++];
        if (!n || n.t !== 'name') fail();
        return { k: 'var', name: n.v };
      }
      if (t.t === 'func') {
        expect('(');
        const args = [];
        if (!isOp(')')) {
          args.push(orExpr());
          while (isOp(',')) { p++; args.push(orExpr()); }
        }
        expect(')');
        if (!FUNCS[t.v]) throw syntaxError(`The string '${src}' is not a valid XPath expression.`);
        return { k: 'call', name: t.v, args };
      }
      fail();
    }
    function predicate() {
      expect('[');
      const e = orExpr();
      expect(']');
      return e;
    }
    const DOS = { axis: 'descendant-or-self', test: { type: 'node' }, preds: [] };
    function locationPath() {
      const steps = [];
      if (isOp('/')) {
        p++;
        const t = peek();
        // A lone "/" selects the root.
        if (t && (t.t === 'name' || t.t === 'axis' || t.t === 'nodetype' || (t.t === 'op' && (t.v === '.' || t.v === '..' || t.v === '@' || t.v === '*')))) {
          steps.push(step());
          relPathRest(steps);
        }
        return { k: 'path', abs: true, steps };
      }
      if (isOp('//')) {
        p++;
        steps.push(DOS);
        steps.push(step());
        relPathRest(steps);
        return { k: 'path', abs: true, steps };
      }
      steps.push(step());
      relPathRest(steps);
      return { k: 'path', abs: false, steps };
    }
    function relPath(steps) {
      // Called with the cursor on "/" or "//".
      relPathRest(steps);
    }
    function relPathRest(steps) {
      for (;;) {
        if (isOp('/')) { p++; steps.push(step()); }
        else if (isOp('//')) { p++; steps.push(DOS); steps.push(step()); }
        else return;
      }
    }
    function step() {
      if (isOp('.')) { p++; return { axis: 'self', test: { type: 'node' }, preds: [] }; }
      if (isOp('..')) { p++; return { axis: 'parent', test: { type: 'node' }, preds: [] }; }
      let axis = 'child';
      const t = peek();
      if (!t) fail();
      if (t.t === 'axis') {
        if (!AXES.has(t.v)) fail();
        axis = t.v;
        p += 2;
      } else if (t.t === 'op' && t.v === '@') { axis = 'attribute'; p++; }
      const test = nodeTest();
      const preds = [];
      while (isOp('[')) preds.push(predicate());
      return { axis, test, preds };
    }
    function nodeTest() {
      const t = toks[p++];
      if (!t) fail();
      if (t.t === 'op' && t.v === '*') return { type: 'any' };
      if (t.t === 'mul') return { type: 'any' };
      if (t.t === 'nodetype') {
        expect('(');
        let lit = null;
        if (t.v === 'processing-instruction' && peek() && peek().t === 'str') lit = toks[p++].v;
        expect(')');
        if (t.v === 'node') return { type: 'node' };
        if (t.v === 'text') return { type: 'text' };
        if (t.v === 'comment') return { type: 'comment' };
        return { type: 'pi', name: lit };
      }
      if (t.t === 'name' || t.t === 'operator' || t.t === 'func' || t.t === 'axis') {
        const v = t.v;
        const i = v.indexOf(':');
        if (i >= 0) {
          const prefix = v.slice(0, i), local = v.slice(i + 1);
          return local === '*' ? { type: 'nsany', prefix } : { type: 'qname', prefix, name: local };
        }
        return { type: 'name', name: v };
      }
      fail();
    }
    const e = orExpr();
    if (p !== toks.length) fail();
    return e;
  }

  // ------------------------------------------------------------- node model
  const isAttr = (n) => n && n.nodeType === 2;
  function parentOf(n) { return isAttr(n) ? n.ownerElement : n.parentNode; }
  function rootOf(n) {
    let r = isAttr(n) ? n.ownerElement || n : n;
    while (r.parentNode) r = r.parentNode;
    return r;
  }
  function stringValue(n) {
    const t = n.nodeType;
    if (t === 1 || t === 9 || t === 11) {
      let s = '';
      const walk = (x) => {
        for (let c = x.firstChild; c; c = c.nextSibling) {
          const ct = c.nodeType;
          if (ct === 3 || ct === 4) s += c.data;
          else if (ct === 1) walk(c);
        }
      };
      walk(n);
      return s;
    }
    if (t === 2) return n.value;
    return n.data !== undefined ? n.data : (n.nodeValue || '');
  }
  function isHTMLDoc(n) {
    const d = n.nodeType === 9 ? n : n.ownerDocument;
    return !!d && !(d instanceof global.XMLDocument) && d.contentType !== 'application/xml' && d.contentType !== 'image/svg+xml';
  }
  // Document order: index nodes lazily per evaluation.
  function sortNodes(nodes) {
    if (nodes.length < 2) return nodes;
    const order = new Map();
    let counter = 0;
    const roots = new Set();
    for (const n of nodes) roots.add(rootOf(n));
    const visit = (x) => {
      order.set(x, counter++);
      if (x.nodeType === 1) {
        const attrs = x.attributes;
        if (attrs) for (let i = 0; i < attrs.length; i++) order.set(attrs[i], counter++);
      }
      for (let c = x.firstChild; c; c = c.nextSibling) visit(c);
    };
    for (const r of roots) visit(r);
    const key = (n) => { const k = order.get(n); return k === undefined ? Infinity : k; };
    return nodes.slice().sort((a, b) => key(a) - key(b));
  }
  function uniqSorted(nodes) {
    if (nodes.length < 2) return nodes;
    const seen = new Set();
    const out = [];
    for (const n of nodes) if (!seen.has(n)) { seen.add(n); out.push(n); }
    return sortNodes(out);
  }

  function descendants(n, out, test) {
    for (let c = n.firstChild; c; c = c.nextSibling) {
      if (test(c)) out.push(c);
      if (c.firstChild) descendants(c, out, test);
    }
  }
  function axisNodes(axis, n, test) {
    const out = [];
    switch (axis) {
      case 'child':
        if (!isAttr(n)) for (let c = n.firstChild; c; c = c.nextSibling) if (test(c)) out.push(c);
        break;
      case 'descendant':
        if (!isAttr(n)) descendants(n, out, test);
        break;
      case 'descendant-or-self':
        if (test(n)) out.push(n);
        if (!isAttr(n)) descendants(n, out, test);
        break;
      case 'self':
        if (test(n)) out.push(n);
        break;
      case 'parent': {
        const p = parentOf(n);
        if (p && test(p)) out.push(p);
        break;
      }
      case 'ancestor':
      case 'ancestor-or-self': {
        if (axis === 'ancestor-or-self' && test(n)) out.push(n);
        for (let p = parentOf(n); p; p = p.parentNode) if (test(p)) out.push(p);
        break;
      }
      case 'following-sibling':
        if (!isAttr(n)) for (let s = n.nextSibling; s; s = s.nextSibling) if (test(s)) out.push(s);
        break;
      case 'preceding-sibling':
        if (!isAttr(n)) for (let s = n.previousSibling; s; s = s.previousSibling) if (test(s)) out.push(s);
        break;
      case 'following': {
        let x = isAttr(n) ? n.ownerElement : n;
        if (!x) break;
        if (isAttr(n)) { if (x.firstChild) descendants(x, out, test); }
        for (; x; x = x.parentNode) {
          for (let s = x.nextSibling; s; s = s.nextSibling) {
            if (test(s)) out.push(s);
            descendants(s, out, test);
          }
        }
        break;
      }
      case 'preceding': {
        let x = isAttr(n) ? n.ownerElement : n;
        if (!x) break;
        const anc = new Set();
        for (let p = x.parentNode; p; p = p.parentNode) anc.add(p);
        const all = [];
        descendants(rootOf(x), all, () => true);
        for (let i = all.length - 1; i >= 0; i--) {
          const c = all[i];
          if (c === x) continue;
          if (anc.has(c)) continue;
          // Only nodes before x in document order.
          if (c.compareDocumentPosition && (x.compareDocumentPosition(c) & 2) && !(x.compareDocumentPosition(c) & 8)) { if (test(c)) out.push(c); }
        }
        break;
      }
      case 'attribute': {
        if (n.nodeType !== 1) break;
        const attrs = n.attributes;
        for (let i = 0; i < attrs.length; i++) {
          const a = attrs[i];
          const nm = a.name;
          if (nm === 'xmlns' || nm.startsWith('xmlns:')) continue;
          if (test(a)) out.push(a);
        }
        break;
      }
      case 'namespace':
        break;
    }
    return out;
  }
  const REVERSE = new Set(['ancestor', 'ancestor-or-self', 'preceding', 'preceding-sibling']);

  function makeTest(test, axis, ctx) {
    const principal = axis === 'attribute' ? 2 : 1;
    switch (test.type) {
      case 'node': return () => true;
      case 'text': return (n) => n.nodeType === 3 || n.nodeType === 4;
      case 'comment': return (n) => n.nodeType === 8;
      case 'pi': return (n) => n.nodeType === 7 && (test.name == null || n.target === test.name);
      case 'any': return (n) => n.nodeType === principal;
      case 'name': {
        const name = test.name;
        const lower = asciiLower(name);
        if (principal === 2) {
          return (n) => n.nodeType === 2 && (n.name === name || (ctx.html && n.ownerElement && n.ownerElement.namespaceURI === HTML_NS && n.name === lower));
        }
        // Unprefixed names match null-namespace elements, and in HTML
        // documents HTML elements by lowercased local name.
        return (n) => {
          if (n.nodeType !== 1) return false;
          const ns = n.namespaceURI;
          if (ctx.html && ns === HTML_NS) return n.localName === lower;
          return n.localName === name && !ns;
        };
      }
      case 'qname':
      case 'nsany': {
        const uri = test.uri;
        if (test.type === 'nsany') return (n) => n.nodeType === principal && (n.namespaceURI || null) === uri;
        return (n) => n.nodeType === principal && n.localName === test.name && (n.namespaceURI || null) === uri;
      }
    }
    return () => false;
  }

  // ------------------------------------------------------------ evaluation
  const isNodeSet = Array.isArray;
  function toStr(v) {
    if (isNodeSet(v)) return v.length ? stringValue(v[0]) : '';
    if (typeof v === 'string') return v;
    if (typeof v === 'boolean') return v ? 'true' : 'false';
    return numToStr(v);
  }
  function numToStr(n) {
    if (n !== n) return 'NaN';
    if (n === Infinity) return 'Infinity';
    if (n === -Infinity) return '-Infinity';
    if (n === 0) return '0';
    if (Number.isInteger(n) && Math.abs(n) < 1e21) return String(n);
    let s = String(n);
    if (s.indexOf('e') >= 0) {
      // Expand exponent notation; XPath numbers never use it.
      const neg = n < 0;
      s = Math.abs(n).toFixed(20).replace(/0+$/, '').replace(/\.$/, '');
      if (neg) s = '-' + s;
    }
    return s;
  }
  function toNum(v) {
    if (typeof v === 'number') return v;
    if (typeof v === 'boolean') return v ? 1 : 0;
    const s = toStr(v);
    return /^[\t\n\r ]*-?(\d+(\.\d*)?|\.\d+)[\t\n\r ]*$/.test(s) ? parseFloat(s) : NaN;
  }
  function toBool(v) {
    if (isNodeSet(v)) return v.length > 0;
    if (typeof v === 'string') return v.length > 0;
    if (typeof v === 'number') return v !== 0 && v === v;
    return !!v;
  }
  function compareAtoms(op, a, b) {
    if (op === '=' || op === '!=') {
      let r;
      if (typeof a === 'boolean' || typeof b === 'boolean') r = toBool(a) === toBool(b);
      else if (typeof a === 'number' || typeof b === 'number') r = toNum(a) === toNum(b);
      else r = toStr(a) === toStr(b);
      return op === '=' ? r : !r;
    }
    const x = toNum(a), y = toNum(b);
    switch (op) {
      case '<': return x < y;
      case '>': return x > y;
      case '<=': return x <= y;
      default: return x >= y;
    }
  }
  function compare(op, a, b) {
    const an = isNodeSet(a), bn = isNodeSet(b);
    if (an && bn) {
      const bs = b.map(stringValue);
      for (const x of a) { const xs = stringValue(x); for (const ys of bs) if (compareAtoms(op, xs, ys)) return true; }
      return false;
    }
    if (an || bn) {
      const set = an ? a : b, other = an ? b : a;
      if (typeof other === 'boolean') return an ? compareAtoms(op, toBool(set), other) : compareAtoms(op, other, toBool(set));
      for (const n of set) {
        const sv = typeof other === 'number' ? toNum(stringValue(n)) : stringValue(n);
        if (an ? compareAtoms(op, sv, other) : compareAtoms(op, other, sv)) return true;
      }
      return false;
    }
    return compareAtoms(op, a, b);
  }

  function usesPosition(e) {
    if (!e || typeof e !== 'object') return false;
    if (e.k === 'call' && (e.name === 'position' || e.name === 'last')) return true;
    if (e.k === 'lit' && typeof e.v === 'number') return true;
    if (e.k === 'arith' || e.k === 'neg') return true;
    if (e.k === 'call' && NUMERIC_FUNCS.has(e.name)) return true;
    if (e.k === 'path') return false; // inner paths have their own context
    for (const key of ['l', 'r', 'e']) if (e[key] && usesPosition(e[key])) return true;
    if (e.args) for (const a of e.args) if (usesPosition(a)) return true;
    return false;
  }
  const NUMERIC_FUNCS = new Set(['count', 'sum', 'number', 'floor', 'ceiling', 'round', 'string-length', 'position', 'last']);

  function applyPreds(nodes, preds, ctx, reverse) {
    for (const pred of preds) {
      const size = nodes.length;
      const kept = [];
      for (let i = 0; i < size; i++) {
        const pos = reverse ? size - i : i + 1;
        const v = evaluate(pred, { node: nodes[i], pos, size, env: ctx.env });
        if (typeof v === 'number' ? v === pos : toBool(v)) kept.push(nodes[i]);
      }
      nodes = kept;
    }
    return nodes;
  }

  function evalSteps(start, steps, env) {
    let set = start;
    for (let si = 0; si < steps.length; si++) {
      let st = steps[si];
      // "//name" without positional predicates: one descendant walk per node.
      if (st.axis === 'descendant-or-self' && st.test.type === 'node' && !st.preds.length) {
        const nx = steps[si + 1];
        if (nx && nx.axis === 'child' && !nx.preds.some(usesPosition)) {
          st = { axis: 'descendant', test: nx.test, preds: nx.preds };
          si++;
        }
      }
      const test = makeTest(st.test, st.axis, env);
      const reverse = REVERSE.has(st.axis);
      let out = [];
      for (const n of set) {
        let nodes = axisNodes(st.axis, n, test);
        if (st.preds.length) {
          // axisNodes returns reverse axes nearest-first, forward axes in order.
          nodes = applyPreds(nodes, st.preds, { env }, false);
        }
        if (set.length === 1 && !reverse) { out = nodes; break; }
        for (const x of nodes) out.push(x);
      }
      set = (set.length === 1 && !reverse && st.axis !== 'preceding') ? out : uniqSorted(out);
      if (reverse && set.length > 1) set = uniqSorted(set);
    }
    return set;
  }

  function evaluate(e, c) {
    switch (e.k) {
      case 'lit': return e.v;
      case 'var': throw new DOMException("Failed to execute 'evaluate': XPath variables are not supported.", 'SyntaxError');
      case 'or': return toBool(evaluate(e.l, c)) || toBool(evaluate(e.r, c));
      case 'and': return toBool(evaluate(e.l, c)) && toBool(evaluate(e.r, c));
      case 'cmp': return compare(e.op, evaluate(e.l, c), evaluate(e.r, c));
      case 'arith': {
        const a = toNum(evaluate(e.l, c)), b = toNum(evaluate(e.r, c));
        switch (e.op) {
          case '+': return a + b;
          case '-': return a - b;
          case '*': return a * b;
          case 'div': return a / b;
          default: return a % b;
        }
      }
      case 'neg': return -toNum(evaluate(e.e, c));
      case 'union': {
        const a = evaluate(e.l, c), b = evaluate(e.r, c);
        if (!isNodeSet(a) || !isNodeSet(b)) throw typeError('The union operator requires node-sets.');
        return uniqSorted(a.concat(b));
      }
      case 'filter': {
        const v = evaluate(e.e, c);
        if (!isNodeSet(v)) throw typeError('Predicates require a node-set.');
        return applyPreds(v, e.preds, c, false);
      }
      case 'path': {
        let start;
        if (e.from) {
          start = evaluate(e.from, c);
          if (!isNodeSet(start)) throw typeError('A location step requires a node-set.');
        } else if (e.abs) start = [rootOf(c.node)];
        else start = [c.node];
        return evalSteps(start, e.steps, c.env);
      }
      case 'call': return FUNCS[e.name](c, e.args);
    }
    throw syntaxError('bad expression');
  }

  const str = (c, a) => a ? toStr(evaluate(a, c)) : stringValue(c.node);
  const FUNCS = {
    last: (c) => c.size,
    position: (c) => c.pos,
    count: (c, a) => { const v = evaluate(a[0], c); if (!isNodeSet(v)) throw typeError('count() requires a node-set.'); return v.length; },
    id: (c, a) => {
      const v = evaluate(a[0], c);
      const ids = isNodeSet(v) ? v.map(stringValue).join(' ') : toStr(v);
      const doc = rootOf(c.node);
      const out = [];
      for (const id of ids.split(/[\t\n\r ]+/)) {
        if (!id) continue;
        const el = doc.getElementById ? doc.getElementById(id) : (doc.querySelector ? doc.querySelector('[id="' + id.replace(/"/g, '\\"') + '"]') : null);
        if (el) out.push(el);
      }
      return uniqSorted(out);
    },
    'local-name': (c, a) => { const v = a.length ? evaluate(a[0], c) : [c.node]; const n = v[0]; if (!n) return ''; return n.nodeType === 1 || n.nodeType === 2 ? n.localName : n.nodeType === 7 ? n.target : ''; },
    'namespace-uri': (c, a) => { const v = a.length ? evaluate(a[0], c) : [c.node]; const n = v[0]; return n && (n.nodeType === 1 || n.nodeType === 2) ? (n.namespaceURI || '') : ''; },
    name: (c, a) => {
      const v = a.length ? evaluate(a[0], c) : [c.node];
      const n = v[0];
      if (!n) return '';
      if (n.nodeType === 1) return n.namespaceURI === HTML_NS && c.env.html ? n.localName : (n.prefix ? n.prefix + ':' + n.localName : n.localName);
      if (n.nodeType === 2) return n.name;
      if (n.nodeType === 7) return n.target;
      return '';
    },
    string: (c, a) => str(c, a[0]),
    concat: (c, a) => a.map((x) => toStr(evaluate(x, c))).join(''),
    'starts-with': (c, a) => toStr(evaluate(a[0], c)).startsWith(toStr(evaluate(a[1], c))),
    'ends-with': (c, a) => toStr(evaluate(a[0], c)).endsWith(toStr(evaluate(a[1], c))),
    contains: (c, a) => toStr(evaluate(a[0], c)).indexOf(toStr(evaluate(a[1], c))) >= 0,
    'substring-before': (c, a) => { const s = toStr(evaluate(a[0], c)), t = toStr(evaluate(a[1], c)); const i = s.indexOf(t); return i < 0 ? '' : s.slice(0, i); },
    'substring-after': (c, a) => { const s = toStr(evaluate(a[0], c)), t = toStr(evaluate(a[1], c)); const i = s.indexOf(t); return i < 0 ? '' : s.slice(i + t.length); },
    substring: (c, a) => {
      const s = Array.from(toStr(evaluate(a[0], c)));
      const start = Math.round(toNum(evaluate(a[1], c)));
      const len = a.length > 2 ? Math.round(toNum(evaluate(a[2], c))) : Infinity;
      if (start !== start || len !== len) return '';
      let out = '';
      for (let i = 0; i < s.length; i++) {
        const pos = i + 1;
        if (pos >= start && pos < start + len) out += s[i];
      }
      return out;
    },
    'string-length': (c, a) => Array.from(str(c, a[0])).length,
    'normalize-space': (c, a) => str(c, a[0]).replace(/[\t\n\r ]+/g, ' ').replace(/^ | $/g, ''),
    translate: (c, a) => {
      const s = toStr(evaluate(a[0], c)), from = Array.from(toStr(evaluate(a[1], c))), to = Array.from(toStr(evaluate(a[2], c)));
      let out = '';
      for (const ch of s) {
        const i = from.indexOf(ch);
        if (i < 0) out += ch;
        else if (i < to.length) out += to[i];
      }
      return out;
    },
    boolean: (c, a) => toBool(evaluate(a[0], c)),
    not: (c, a) => !toBool(evaluate(a[0], c)),
    true: () => true,
    false: () => false,
    lang: (c, a) => {
      const want = asciiLower(toStr(evaluate(a[0], c)));
      for (let n = isAttr(c.node) ? c.node.ownerElement : c.node; n; n = n.parentNode) {
        if (n.nodeType !== 1) continue;
        const l = n.getAttributeNS && n.getAttributeNS(XML_NS, 'lang') || n.getAttribute('xml:lang') || (c.env.html ? n.getAttribute('lang') : null);
        if (l != null) { const v = asciiLower(l); return v === want || v.startsWith(want + '-'); }
      }
      return false;
    },
    number: (c, a) => a.length ? toNum(evaluate(a[0], c)) : toNum(stringValue(c.node)),
    sum: (c, a) => { const v = evaluate(a[0], c); if (!isNodeSet(v)) throw typeError('sum() requires a node-set.'); let s = 0; for (const n of v) s += toNum(stringValue(n)); return s; },
    floor: (c, a) => Math.floor(toNum(evaluate(a[0], c))),
    ceiling: (c, a) => Math.ceil(toNum(evaluate(a[0], c))),
    round: (c, a) => { const x = toNum(evaluate(a[0], c)); if (x !== x || !isFinite(x)) return x; if (x < 0 && x >= -0.5) return -0; return Math.floor(x + 0.5); },
  };

  // ----------------------------------------------------------- DOM binding
  // XPathNSResolver is a callback interface: a function is called as is,
  // an object's lookupNamespaceURI is read on every call. Exceptions are
  // reported and the prefix stays unresolved (NamespaceError).
  function resolvePrefix(r, prefix) {
    const nsErr = () => new DOMException(`The namespace prefix '${prefix}' is not defined.`, 'NamespaceError');
    if (r == null) throw nsErr();
    let uri = null;
    try {
      let v;
      if (typeof r === 'function') v = r(prefix);
      else {
        const f = r.lookupNamespaceURI;
        if (typeof f !== 'function') throw new TypeError("Failed to execute 'lookupNamespaceURI' on 'XPathNSResolver': The provided callback is not callable.");
        v = f.call(r, prefix);
      }
      uri = v === null || v === undefined ? null : `${v}`;
    } catch (e) {
      if (typeof global.reportError === 'function') global.reportError(e);
      uri = null;
    }
    if (uri === null) throw nsErr();
    return uri === '' ? null : uri;
  }
  function resolveTests(e, r) {
    if (!e || typeof e !== 'object') return;
    if (Array.isArray(e)) { for (const x of e) resolveTests(x, r); return; }
    if (e.test && (e.test.type === 'qname' || e.test.type === 'nsany')) e.test.uri = resolvePrefix(r, e.test.prefix);
    for (const k of ['l', 'r', 'e', 'from', 'steps', 'preds', 'args']) if (e[k]) resolveTests(e[k], r);
  }

  const RT = { ANY_TYPE: 0, NUMBER_TYPE: 1, STRING_TYPE: 2, BOOLEAN_TYPE: 3, UNORDERED_NODE_ITERATOR_TYPE: 4, ORDERED_NODE_ITERATOR_TYPE: 5, UNORDERED_NODE_SNAPSHOT_TYPE: 6, ORDERED_NODE_SNAPSHOT_TYPE: 7, ANY_UNORDERED_NODE_TYPE: 8, FIRST_ORDERED_NODE_TYPE: 9 };
  const RESULT = Symbol('xpath.result');
  class XPathResult {
    constructor() { throw new TypeError('Illegal constructor'); }
    get resultType() { return this[RESULT].type; }
    get numberValue() { const r = this[RESULT]; if (r.type !== 1) throw typeError("Failed to read the 'numberValue' property from 'XPathResult': The result type is not a number."); return r.value; }
    get stringValue() { const r = this[RESULT]; if (r.type !== 2) throw typeError("Failed to read the 'stringValue' property from 'XPathResult': The result type is not a string."); return r.value; }
    get booleanValue() { const r = this[RESULT]; if (r.type !== 3) throw typeError("Failed to read the 'booleanValue' property from 'XPathResult': The result type is not a boolean."); return r.value; }
    get singleNodeValue() { const r = this[RESULT]; if (r.type !== 8 && r.type !== 9) throw typeError("Failed to read the 'singleNodeValue' property from 'XPathResult': The result type is not a single node."); return r.nodes[0] || null; }
    // Iterators are not invalidated by later mutations (lenient; Chromium throws).
    get invalidIteratorState() { const r = this[RESULT]; return (r.type === 4 || r.type === 5) && r.gen !== domGen(); }
    get snapshotLength() { const r = this[RESULT]; if (r.type !== 6 && r.type !== 7) throw typeError("Failed to read the 'snapshotLength' property from 'XPathResult': The result type is not a snapshot."); return r.nodes.length; }
    iterateNext() {
      const r = this[RESULT];
      if (r.type !== 4 && r.type !== 5) throw typeError("Failed to execute 'iterateNext' on 'XPathResult': The result type is not an iterator.");
      if (r.gen !== domGen()) throw new DOMException("Failed to execute 'iterateNext' on 'XPathResult': The document has mutated since the result was returned.", 'InvalidStateError');
      return r.i < r.nodes.length ? r.nodes[r.i++] : null;
    }
    snapshotItem(i) {
      const r = this[RESULT];
      if (r.type !== 6 && r.type !== 7) throw typeError("Failed to execute 'snapshotItem' on 'XPathResult': The result type is not a snapshot.");
      return r.nodes[i >>> 0] || null;
    }
  }
  Object.assign(XPathResult, RT);
  Object.assign(XPathResult.prototype, RT);
  Object.defineProperty(XPathResult.prototype, Symbol.toStringTag, { value: 'XPathResult', configurable: true });
  function makeResult(value, type, ctxNode) {
    if (type === 0) {
      type = isNodeSet(value) ? 4 : typeof value === 'number' ? 1 : typeof value === 'string' ? 2 : 3;
    }
    const r = Object.create(XPathResult.prototype);
    const st = { type, value: null, nodes: [], i: 0, gen: domGen() };
    switch (type) {
      case 1: st.value = toNum(value); break;
      case 2: st.value = toStr(value); break;
      case 3: st.value = toBool(value); break;
      case 4: case 5: case 6: case 7: case 8: case 9:
        if (!isNodeSet(value)) throw typeError("Failed to execute 'evaluate' on 'Document': The result is not a node set, and therefore cannot be converted to the desired type.");
        st.nodes = type === 8 || type === 9 ? value.slice(0, 1) : value;
        break;
      default:
        throw new DOMException("Failed to execute 'evaluate': The result type is not supported.", 'NotSupportedError');
    }
    Object.defineProperty(r, RESULT, { value: st });
    return r;
  }

  const EXPR = Symbol('xpath.expr');
  class XPathExpression {
    constructor() { throw new TypeError('Illegal constructor'); }
    evaluate(contextNode, type, result) {
      const st = this[EXPR];
      if (!(contextNode instanceof global.Node)) throw typeError("Failed to execute 'evaluate' on 'XPathExpression': parameter 1 is not of type 'Node'.");
      type = type === undefined ? 0 : Number(type) >>> 0;
      const env = { html: isHTMLDoc(contextNode) };
      const v = evaluate(st.ast, { node: contextNode, pos: 1, size: 1, env });
      return makeResult(v, type, contextNode);
    }
  }
  Object.defineProperty(XPathExpression.prototype, Symbol.toStringTag, { value: 'XPathExpression', configurable: true });
  function createExpression(expr, resolver) {
    const ast = parse(String(expr));
    resolveTests(ast, resolver);
    const e = Object.create(XPathExpression.prototype);
    Object.defineProperty(e, EXPR, { value: { ast } });
    return e;
  }
  function createNSResolver(node) { return node; }
  function evaluateXPath(expr, contextNode, resolver, type, result) {
    return createExpression(expr, resolver).evaluate(contextNode, type, result);
  }
  class XPathEvaluator {
    createExpression(expr, resolver) { return createExpression(expr, resolver); }
    createNSResolver(node) { return createNSResolver(node); }
    evaluate(expr, contextNode, resolver, type, result) { return evaluateXPath(expr, contextNode, resolver, type, result); }
  }
  Object.defineProperty(XPathEvaluator.prototype, Symbol.toStringTag, { value: 'XPathEvaluator', configurable: true });

  const DP = global.Document.prototype;
  hidden(DP, 'evaluate', function evaluate(expr, contextNode, resolver, type, result) { return evaluateXPath(expr, contextNode === undefined ? this : contextNode, resolver, type, result); });
  hidden(DP, 'createExpression', function createExpression_(expr, resolver) { return createExpression(expr, resolver); });
  hidden(DP, 'createNSResolver', function createNSResolver_(node) { return createNSResolver(node); });
  hidden(global, 'XPathResult', XPathResult);
  hidden(global, 'XPathExpression', XPathExpression);
  hidden(global, 'XPathEvaluator', XPathEvaluator);
  hidden(global, 'XPathException', function XPathException() { throw new TypeError('Illegal constructor'); });
})(globalThis);
