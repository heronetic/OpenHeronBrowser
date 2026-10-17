#!/usr/bin/env python3
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
"""Score OHB (and Chromium) on EPLS, the Exhaustive Page Layout Stresstest.

  epls.py EPLS_DIR [--sections 01,05] [--width 1280] [--ref-only] [--verbose]

For every sections/NN-*.html page both engines render a full-page screenshot
and report each test case's box (`.t`), its self-check badges (`.ck`) and cover
tests (`.cov`). The page is scored from the pixels:
  badge  PASS when the badge shows more pass-green than fail-red;
  cover  OK when no fail-red is visible in or around the cover box;
  visual the share of pixels in the case's stage that differ from Chromium
         by more than 40 in any channel (stages are compared at their own
         positions in each engine, so a shifted case still lines up).
Chromium results are cached in OUT/epls/ref (delete to refresh).
Writes OUT/epls/report.json and prints a per-section table; --verbose lists
every failing case.
"""
import argparse, glob, json, os, subprocess, sys
from PIL import Image, ImageChops
import common

FAIL = (212, 42, 42)
PASS = (23, 138, 67)

COLLECT = r"""(() => {
  const sx = window.scrollX || 0, sy = window.scrollY || 0;
  const R = e => { const r = e.getBoundingClientRect(); return [r.left + sx, r.top + sy, r.width, r.height]; };
  const cases = [...document.querySelectorAll('.t')].map(t => {
    const st = t.querySelector('.stage');
    return { id: ((t.querySelector('.id') || {}).textContent || '').trim(),
             title: ((t.querySelector('h3') || t.querySelector('.id') || {}).textContent || '').trim().slice(0, 80),
             r: R(t), stage: st ? R(st) : null,
             badges: [...t.querySelectorAll('.ck')].map(R),
             covers: [...t.querySelectorAll('.cov')].map(R) };
  });
  return JSON.stringify({ w: document.documentElement.scrollWidth, h: document.documentElement.scrollHeight, cases });
})()"""


def near(px, c, tol=48):
    return abs(px[0] - c[0]) <= tol and abs(px[1] - c[1]) <= tol and abs(px[2] - c[2]) <= tol


def count(img, rect, colour, exclude=()):
    x, y, w, h = [int(round(v)) for v in rect]
    x0, y0 = max(x, 0), max(y, 0)
    x1, y1 = min(x + w, img.width), min(y + h, img.height)
    if x1 <= x0 or y1 <= y0:
        return 0, 0
    px = img.load()
    n = tot = 0
    for yy in range(y0, y1):
        for xx in range(x0, x1):
            if any(ex[0] <= xx < ex[0] + ex[2] and ex[1] <= yy < ex[1] + ex[3] for ex in exclude):
                continue
            tot += 1
            if near(px[xx, yy], colour):
                n += 1
    return n, tot


def badge_state(img, r):
    inner = (r[0] + 1, r[1] + 1, r[2] - 2, r[3] - 2)
    g, _ = count(img, inner, PASS)
    red, _ = count(img, inner, FAIL)
    if g == 0 and red == 0:
        return 'missing'
    return 'pass' if g > red else 'fail'


def score(img, data):
    out = []
    for c in data['cases']:
        badges = [badge_state(img, b) for b in c['badges']]
        covers = []
        for cv in c['covers']:
            area = (cv[0] - 6, cv[1] - 6, cv[2] + 12, cv[3] + 12)
            red, _ = count(img, area, FAIL, exclude=[(b[0] - 1, b[1] - 1, b[2] + 2, b[3] + 2) for b in c['badges']])
            covers.append(red)
        out.append({'id': c['id'], 'title': c['title'], 'badges': badges, 'covers': covers})
    return out


def render_ohb(url, png, w):
    env = dict(os.environ, OHB_QUIET='1')
    p = subprocess.run(['timeout', '180', common.OHB, '--screenshot', png, url, '--width', str(w), '--height', '900',
                        '--wait', '1500', '--full', '--eval', COLLECT], capture_output=True, text=True, env=env)
    lines = [l for l in p.stdout.splitlines() if l.startswith('{')]
    return json.loads(lines[-1]) if lines else {'cases': []}


def render_chromium(url, png, w):
    p = subprocess.run([sys.executable, os.path.join(common.HERE, 'chromium.py'), url, '--size', f'{w}x900', '--wait', '300',
                        '--full', '--shot', png, '--eval', COLLECT], capture_output=True, text=True)
    lines = [l for l in p.stdout.splitlines() if l.startswith('{')]
    return json.loads(lines[-1]) if lines else {'cases': []}


def stage_diff(ref_img, ohb_img, rs, os_):
    if not rs or not os_:
        return 1.0
    w, h = int(round(rs[2])), int(round(rs[3]))
    if w <= 0 or h <= 0:
        return 0.0
    a = ref_img.crop((int(round(rs[0])), int(round(rs[1])), int(round(rs[0])) + w, int(round(rs[1])) + h))
    b = ohb_img.crop((int(round(os_[0])), int(round(os_[1])), int(round(os_[0])) + w, int(round(os_[1])) + h))
    d = ImageChops.difference(a, b).convert('L').point(lambda v: 255 if v > 40 else 0)
    return sum(1 for v in d.get_flattened_data() if v) / float(w * h)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('epls'); ap.add_argument('--sections'); ap.add_argument('--width', type=int, default=1280)
    ap.add_argument('--ref-only', action='store_true'); ap.add_argument('--verbose', action='store_true')
    ap.add_argument('--visual', type=float, default=0.02, help='stage diff share above which a case counts as visually different')
    a = ap.parse_args()
    out = os.path.join(common.OUT, 'epls')
    os.makedirs(os.path.join(out, 'ref'), exist_ok=True)
    os.makedirs(os.path.join(out, 'ohb'), exist_ok=True)
    files = sorted(glob.glob(os.path.join(a.epls, 'sections', '*.html')))
    if a.sections:
        want = set(a.sections.split(','))
        files = [f for f in files if os.path.basename(f)[:2] in want]
    report = {}
    tot = dict(b=0, bp=0, c=0, cp=0, v=0, vd=0)
    print(f"{'section':44} {'badges':>9} {'covers':>9} {'visual':>9}")
    for f in files:
        name = os.path.basename(f)[:-5]
        url = 'file://' + os.path.abspath(f)
        rpng, rjs = os.path.join(out, 'ref', name + '.png'), os.path.join(out, 'ref', name + '.json')
        if not os.path.exists(rjs):
            json.dump(render_chromium(url, rpng, a.width), open(rjs, 'w'))
        ref = json.load(open(rjs))
        ref_img = Image.open(rpng).convert('RGB')
        ref_score = score(ref_img, ref)
        if a.ref_only:
            bad = [(c['id'], c['badges'], c['covers']) for c in ref_score if any(s != 'pass' for s in c['badges']) or any(c['covers'])]
            print(f'{name:44} chromium failures: {bad}')
            continue
        opng = os.path.join(out, 'ohb', name + '.png')
        ohb = render_ohb(url, opng, a.width)
        json.dump(ohb, open(os.path.join(out, 'ohb', name + '.json'), 'w'))
        ohb_img = Image.open(opng).convert('RGB') if os.path.exists(opng) else Image.new('RGB', (a.width, 900), 'white')
        osc = score(ohb_img, ohb)
        by_id = {c['id']: (c, s) for c, s in zip(ohb.get('cases', []), osc)}
        rows = []
        nb = nbp = nc = ncp = nv = nvd = 0
        for rc, rs in zip(ref['cases'], ref_score):
            oc, os_ = by_id.get(rc['id'], (None, None))
            ob = os_['badges'] if os_ else ['missing'] * len(rs['badges'])
            ocv = os_['covers'] if os_ else [999] * len(rs['covers'])
            vis = stage_diff(ref_img, ohb_img, rc['stage'], oc['stage'] if oc else None)
            nb += len(rs['badges']); nbp += sum(1 for s in ob if s == 'pass')
            nc += len(rs['covers']); ncp += sum(1 for r in ocv if r <= 3)
            nv += 1; nvd += 1 if vis > a.visual else 0
            rows.append({'id': rc['id'], 'title': rc['title'], 'badges': ob, 'covers': ocv, 'visual': round(vis, 4)})
        report[name] = rows
        tot['b'] += nb; tot['bp'] += nbp; tot['c'] += nc; tot['cp'] += ncp; tot['v'] += nv; tot['vd'] += nvd
        print(f'{name:44} {nbp:>4}/{nb:<4} {ncp:>4}/{nc:<4} {nv - nvd:>4}/{nv:<4}')
        if a.verbose:
            for r in rows:
                bad = [i for i, s in enumerate(r['badges']) if s != 'pass']
                badc = [i for i, v in enumerate(r['covers']) if v > 3]
                if bad or badc or r['visual'] > a.visual:
                    print(f"    {r['id']:6} {r['title'][:46]:46} badges✗{bad} covers✗{badc} visual {r['visual']:.3f}")
    if not a.ref_only:
        print(f"{'TOTAL':44} {tot['bp']:>4}/{tot['b']:<4} {tot['cp']:>4}/{tot['c']:<4} {tot['v'] - tot['vd']:>4}/{tot['v']:<4}")
        json.dump(report, open(os.path.join(out, 'report.json'), 'w'), indent=1)


if __name__ == '__main__':
    main()
