#!/usr/bin/env python3
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
"""Compare element boxes (getBoundingClientRect) between Chromium and OHB.

  boxes.py NAME|- URL 'css selector' [--size 1280x900] [--extra 'JS using e (element) and c (computed style)']

NAME selects the web archive (both engines replay offline); '-' loads URL directly
(for file:// repro pages). Lines starting with '=' agree; C/O pairs show Chromium
and OHB where they differ. The first mismatching element is usually the lead.
"""
import argparse, json, os, subprocess, sys
import common

ap = argparse.ArgumentParser()
ap.add_argument('name'); ap.add_argument('url'); ap.add_argument('selector')
ap.add_argument('--size', default='1280x900'); ap.add_argument('--extra', default="''")
ap.add_argument('--max', type=int, default=60); ap.add_argument('--wait', type=int, default=1500)
a = ap.parse_args()
js = ("(() => { const out=[]; for (const e of document.querySelectorAll(%s)) { const r=e.getBoundingClientRect(); "
      "const c=getComputedStyle(e); const cn=(e.className&&e.className.baseVal!==undefined)?e.className.baseVal:String(e.className||''); "
      "out.push(e.tagName.toLowerCase()+(cn?'.'+cn.split(' ').filter(Boolean).slice(0,3).join('.'):'')+' ['+Math.round(r.x)+','+Math.round(r.y)+' '"
      "+Math.round(r.width)+'x'+Math.round(r.height)+'] '+(%s)); if (out.length>=%d) break; } return out.join('\\n'); })()") % (json.dumps(a.selector), a.extra, a.max)
w, h = a.size.split('x')
chrom = [sys.executable, os.path.join(common.HERE, 'chromium.py'), a.url, '--eval', js, '--size', a.size, '--wait', str(a.wait)]
env = dict(os.environ, OHB_QUIET='1')
if a.name != '-':
    chrom += ['--archive', a.name, '--offline']
    env.update(OHB_ARCHIVE=common.archive_dir(a.name), OHB_ARCHIVE_ONLY='1')
ref = subprocess.run(chrom, capture_output=True, text=True).stdout.strip().split('\n')
ohb = subprocess.run(['timeout', '180', common.OHB, '--screenshot', os.devnull + '.png', a.url, '--width', w, '--height', h,
                      '--wait', str(a.wait), '--eval', js], capture_output=True, text=True, env=env).stdout.strip().split('\n')
for i in range(max(len(ref), len(ohb))):
    r = ref[i] if i < len(ref) else ''; o = ohb[i] if i < len(ohb) else ''
    print('  = ' + r if r == o else 'C ' + r + '\nO ' + o)
