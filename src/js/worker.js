// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// HeronScript worker scope: turns the prelude's window into a
// DedicatedWorkerGlobalScope (no document; postMessage to the page;
// importScripts; fetch and XHR over blocking requests on the worker thread).
(function (global) {
  'use strict';
  const N = global.__n_worker || global.__n;
  try { delete global.__n_worker; } catch (_) {}
  const workerURL = global.__ohb_workerURL;
  const ser = global.__ohb_cloneSer, deser = global.__ohb_cloneDeser;
  const runScript = global.__ohb_runScript;
  // Events the user agent fires are trusted (isTrusted), unlike dispatchEvent().
  const TRUSTED = (global.__ohb_builtin && global.__ohb_builtin.dispatchTrusted) || ((t, ev) => t.dispatchEvent(ev));

  // No DOM in workers.
  for (const k of ['document', 'window', 'parent', 'top', 'frames', 'opener', 'frameElement', 'localStorage', 'sessionStorage', 'alert', 'confirm', 'prompt', 'print', 'open',
    'customElements', 'webkitURL', 'getSelection', 'getComputedStyle', 'matchMedia', 'scrollTo', 'scroll', 'scrollBy', 'visualViewport', 'styleMedia', 'history', 'screen', 'devicePixelRatio',
    'innerWidth', 'innerHeight', 'outerWidth', 'outerHeight', 'scrollX', 'scrollY', 'pageXOffset', 'pageYOffset', 'requestIdleCallback', 'cancelIdleCallback',
    'Node', 'Element', 'HTMLElement', 'Document', 'Image', 'Option', 'Audio', 'DOMParser', 'XMLSerializer', 'MutationObserver', 'IntersectionObserver', 'ResizeObserver',
    'WebKitCSSMatrix', 'SVGMatrix', 'SVGPoint', 'SVGRect', 'DOMRectList',
    'SharedWorker', 'Navigator', 'HTMLCanvasElement', 'CanvasRenderingContext2D', 'BeforeUnloadEvent', 'PopStateEvent', 'HashChangeEvent', 'PageTransitionEvent',
    'DOMImplementation', 'UIEvent', 'FocusEvent', 'MouseEvent', 'WheelEvent', 'InputEvent', 'KeyboardEvent', 'CompositionEvent', 'VTTCue', 'VTTRegion']) {
    try { delete global[k]; } catch (_) {}
    try { if (k in global) Object.defineProperty(global, k, { value: undefined, configurable: true, writable: true }); } catch (_) {}
  }
  Object.defineProperty(global, 'self', { value: global, configurable: true, writable: true });
  // Matrices have no CSS stringifier or setMatrixValue() in workers.
  try { delete global.DOMMatrixReadOnly.prototype.toString; delete global.DOMMatrix.prototype.setMatrixValue; } catch (_) {}
  // The global is a DedicatedWorkerGlobalScope (→ WorkerGlobalScope → EventTarget).
  const illegal = function () { throw new TypeError('Illegal constructor'); };
  const iface = (name, parentProto) => {
    const F = { [name]: function () { illegal(); } }[name];
    F.prototype = Object.create(parentProto);
    Object.defineProperty(F.prototype, 'constructor', { value: F, writable: true, configurable: true });
    Object.defineProperty(F.prototype, Symbol.toStringTag, { value: name, configurable: true });
    Object.defineProperty(global, name, { value: F, writable: true, configurable: true });
    return F;
  };
  const SHARED = !!global.__ohb_sharedWorker;
  try { delete global.__ohb_sharedWorker; } catch (_) {}
  const WGS = iface('WorkerGlobalScope', EventTarget.prototype);
  const DWGS = iface(SHARED ? 'SharedWorkerGlobalScope' : 'DedicatedWorkerGlobalScope', WGS.prototype);
  try { Object.setPrototypeOf(global, DWGS.prototype); } catch (_) {}
  for (const k of ['Window', 'HTMLDocument', 'Location', 'event']) { try { delete global[k]; } catch (_) {} }
  Object.defineProperty(global, 'name', { value: global.__ohb_workerName || '', configurable: true, writable: true });

  // WorkerLocation.
  const loc = new URL(workerURL);
  const location = {};
  for (const k of ['href', 'protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin']) location[k] = loc[k];
  location.toString = () => loc.href;
  Object.defineProperty(global, 'location', { value: location, configurable: true, writable: false });
  Object.defineProperty(global, 'origin', { value: loc.origin, configurable: true, writable: true });

  // Event handlers on the global scope.
  for (const t of SHARED ? ['connect', 'error', 'unhandledrejection', 'rejectionhandled'] : ['message', 'messageerror', 'error', 'unhandledrejection', 'rejectionhandled']) {
    Object.defineProperty(global, 'on' + t, { get() { return global['_on' + t] || null; }, set(f) { global['_on' + t] = f; }, configurable: true });
  }

  const ports = global.__ohb_ports;
  global.postMessage = function (data, transfer) {
    const t = Array.isArray(transfer) ? transfer : (transfer && typeof transfer === 'object' && transfer.transfer ? Array.from(transfer.transfer) : []);
    const s = ser(data, t);
    N.workerPost(s, ports ? ports.take(t) : []);
  };
  if (SHARED) { try { delete global.postMessage; } catch (_) {} }
  global.close = function () { N.workerClose(); };
  global.importScripts = function (...urls) {
    for (const u of urls) {
      const r = N.fetchSync(String(u), 'GET', [], null);
      if (!(r[0] >= 200 && r[0] < 300)) throw new DOMException("Failed to execute 'importScripts': The script at '" + u + "' failed to load.", 'NetworkError');
      // A classic script of its own: top-level let/const join the global scope.
      let path = String(u);
      try { path = new URL(path, global.location.href).href; } catch (_) {}
      if (typeof runScript === 'function') runScript(new TextDecoder().decode(r[4]), path);
      else (0, eval)(new TextDecoder().decode(r[4]));
    }
  };

  // fetch() and XMLHttpRequest come from the prelude (blocking requests in workers).

  // Messages from the page.
  global.__ohb_workerOnMessage = function (data, ids) {
    let v;
    try { v = deser(data); } catch (e) { TRUSTED(global, new MessageEvent('messageerror', { data: null })); return; }
    const evPorts = ports ? ports.forEvent(data, ids) : [];
    if (SHARED) {
      // A new SharedWorker object connected: its port arrives with `connect`.
      if (v && v.__ohb_connect) {
        const ev = new MessageEvent('connect', { data: '', ports: evPorts, source: evPorts[0] || null });
        TRUSTED(global, ev);
      }
      return;
    }
    const ev = new MessageEvent('message', { data: v, ports: evPorts });
    TRUSTED(global, ev);
  };
  // Uncaught errors go to the Worker object on the page.
  global.addEventListener('error', (ev) => { if (!ev.defaultPrevented) N.workerError(String(ev.message || ev.error)); });
})(globalThis);
