// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// testdriver-vendor.js for OpenHeronBrowser.
//
// Implements web-platform-tests' `test_driver_internal` on top of the
// `__ohbTestDriver` object that OHB exposes only when started with OHB_WPT=1.
// Input goes through OHB's real input paths (hit testing, focus, default
// actions), so the resulting events are trusted.
(function () {
  'use strict';
  const D = window.__ohbTestDriver;
  const ti = window.test_driver_internal;
  if (!D || !ti) return;
  ti.in_automation = true;

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // WebDriver key codepoints (https://w3c.github.io/webdriver/#keyboard-actions).
  const KEYS = {
    '': 'Unidentified', '': 'Cancel', '': 'Help', '': 'Backspace',
    '': 'Tab', '': 'Clear', '': 'Enter', '': 'Enter', '': 'Shift',
    '': 'Control', '': 'Alt', '': 'Pause', '': 'Escape', '': ' ',
    '': 'PageUp', '': 'PageDown', '': 'End', '': 'Home',
    '': 'ArrowLeft', '': 'ArrowUp', '': 'ArrowRight', '': 'ArrowDown',
    '': 'Insert', '': 'Delete', '': ';', '': '=',
    '': '0', '': '1', '': '2', '': '3', '': '4', '': '5',
    '': '6', '': '7', '': '8', '': '9', '': '*', '': '+',
    '': ',', '': '-', '': '.', '': '/',
    '': 'F1', '': 'F2', '': 'F3', '': 'F4', '': 'F5', '': 'F6',
    '': 'F7', '': 'F8', '': 'F9', '': 'F10', '': 'F11', '': 'F12',
    '': 'Meta', '': 'ZenkakuHankaku',
    '': 'Shift', '': 'Control', '': 'Alt', '': 'Meta',
    '': 'PageUp', '': 'PageDown', '': 'End', '': 'Home',
    '': 'ArrowLeft', '': 'ArrowUp', '': 'ArrowRight', '': 'ArrowDown',
    '': 'Insert', '': 'Delete',
  };
  const MODS = { Shift: 1, Control: 2, Alt: 4, Meta: 8 };
  let mods = 0;
  const pointer = { x: 0, y: 0 };

  async function key(value, down) {
    const name = KEYS[value] || value;
    if (MODS[name]) {
      if (down) mods |= MODS[name]; else mods &= ~MODS[name];
      return;
    }
    if (!down) return;
    let text = '';
    if ([...name].length === 1) text = (mods & 1) ? name.toUpperCase() : name;
    D.input('key', name, text, 0, 0, mods);
    await D.sync();
  }

  // Viewport coordinates of an action origin.
  function originPoint(origin, x, y) {
    x = x || 0; y = y || 0;
    if (origin === undefined || origin === 'viewport') return [x, y];
    if (origin === 'pointer') return [pointer.x + x, pointer.y + y];
    if (origin && typeof origin.getClientRects === 'function') {
      const r = origin.getClientRects()[0];
      if (!r) throw new Error('move target out of bounds');
      const l = Math.max(0, r.left), rt = Math.min(innerWidth, r.right);
      const t = Math.max(0, r.top), b = Math.min(innerHeight, r.bottom);
      return [Math.floor((l + rt) / 2) + x, Math.floor((t + b) / 2) + y];
    }
    return [x, y];
  }

  ti.click = async function (element, coords) {
    const { x, y } = coords;
    D.input('move', x, y, 0, 0, mods);
    D.input('down', x, y, 0, 0, mods);
    await D.sync();
    D.input('up', x, y, 0, 0, mods);
    await D.sync();
    pointer.x = x; pointer.y = y;
  };

  ti.send_keys = async function (element, keys) {
    if (element.ownerDocument.activeElement !== element && typeof element.focus === 'function') {
      element.focus();
      // Element Send Keys puts the caret after existing content.
      if (typeof element.setSelectionRange === 'function' && typeof element.value === 'string') {
        try { element.setSelectionRange(element.value.length, element.value.length); } catch (e) {}
      }
    }
    for (const ch of keys) {
      if (ch === '') { mods = 0; continue; }
      const name = KEYS[ch] || ch;
      if (MODS[name]) { mods ^= MODS[name]; continue; }
      await key(ch, true);
    }
    mods = 0;
  };

  ti.action_sequence = async function (actions, context) {
    const ticks = Math.max(0, ...actions.map((s) => (s.actions || []).length));
    for (let i = 0; i < ticks; i++) {
      let duration = 0;
      for (const src of actions) {
        const a = (src.actions || [])[i];
        if (!a) continue;
        if (a.type === 'pause') { duration = Math.max(duration, a.duration || 0); continue; }
        if (src.type === 'key') {
          await key(a.value, a.type === 'keyDown');
        } else if (src.type === 'pointer') {
          if (a.type === 'pointerMove') {
            const [x, y] = originPoint(a.origin, a.x, a.y);
            duration = Math.max(duration, a.duration || 0);
            D.input('move', x, y, 0, 0, mods);
            pointer.x = x; pointer.y = y;
          } else if (a.type === 'pointerDown') {
            D.input('down', pointer.x, pointer.y, a.button || 0, 0, mods);
          } else if (a.type === 'pointerUp') {
            D.input('up', pointer.x, pointer.y, a.button || 0, 0, mods);
          }
        } else if (src.type === 'wheel' && a.type === 'scroll') {
          const [x, y] = originPoint(a.origin, a.x, a.y);
          duration = Math.max(duration, a.duration || 0);
          D.input('wheel', x, y, a.deltaX || 0, a.deltaY || 0, mods);
        }
      }
      await D.sync();
      if (duration) await sleep(Math.min(duration, 1000));
    }
    D.input('wheelend', 0, 0, 0, 0, 0);
  };

  ti.set_test_context = function () {};
  ti.get_window_rect = async () => ({ x: screenX || 0, y: screenY || 0, width: outerWidth, height: outerHeight });
  ti.set_window_rect = async () => {};
  ti.minimize_window = async () => ({ x: 0, y: 0, width: outerWidth, height: outerHeight });
  ti.delete_all_cookies = async () => {
    for (const c of document.cookie.split(';')) {
      const n = c.split('=')[0].trim();
      if (n) document.cookie = n + '=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';
    }
  };
  const cookies = () => document.cookie.split(';').map((c) => c.trim()).filter(Boolean).map((c) => {
    const i = c.indexOf('=');
    return { name: i < 0 ? '' : c.slice(0, i), value: i < 0 ? c : c.slice(i + 1), path: '/', domain: location.hostname, secure: false, httpOnly: false };
  });
  ti.get_all_cookies = async () => cookies();
  ti.get_named_cookie = async (name) => {
    const c = cookies().find((x) => x.name === name);
    if (!c) throw new Error('no such cookie');
    return c;
  };
})();
