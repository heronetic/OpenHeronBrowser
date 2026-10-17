#!/usr/bin/env python3
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
"""Render a page in Chromium and OHB from the same web archive and compare.

  compare.py NAME URL [--size 1280x900] [--wait MS] [--record]

--record fetches live (recording into OUT/arch/NAME); otherwise both engines
replay offline. Writes OUT/shots/{ref,ohb,cmp}_NAME.png and prints the number
of pixels that differ by more than 40 in any channel (text anti-aliasing alone
accounts for a few hundred, so look at where differences are, not only how many).
"""
import argparse, os, subprocess, sys
from PIL import Image, ImageChops
import common

ap = argparse.ArgumentParser()
ap.add_argument('name'); ap.add_argument('url')
ap.add_argument('--size', default='1280x900'); ap.add_argument('--wait', type=int, default=3000)
ap.add_argument('--record', action='store_true')
a = ap.parse_args()
w, h = a.size.split('x')
shots = os.path.join(common.OUT, 'shots'); os.makedirs(shots, exist_ok=True)
ref, ohb, cmp = (os.path.join(shots, f'{k}_{a.name}.png') for k in ('ref', 'ohb', 'cmp'))
here = common.HERE
chrom = [sys.executable, os.path.join(here, 'chromium.py'), a.url, '--shot', ref, '--size', a.size, '--wait', str(a.wait), '--archive', a.name]
if not a.record:
    chrom.append('--offline')
subprocess.run(chrom, check=False)
env = dict(os.environ, OHB_QUIET='1', OHB_ARCHIVE=common.archive_dir(a.name), OHB_ARCHIVE_ONLY='0' if a.record else '1')
subprocess.run(['timeout', '180', common.OHB, '--screenshot', ohb, a.url, '--width', w, '--height', h, '--wait', str(a.wait)], env=env, check=False)
A = Image.open(ref).convert('RGB'); B = Image.open(ohb).convert('RGB')
side = Image.new('RGB', (A.width + B.width + 10, max(A.height, B.height)), (255, 0, 255))
side.paste(A, (0, 0)); side.paste(B, (A.width + 10, 0)); side.save(cmp)
if A.size == B.size:
    d = ImageChops.difference(A, B).convert('L').point(lambda v: 255 if v > 40 else 0)
    print(f'{a.name}: {sum(1 for v in d.get_flattened_data() if v)} px differ, bbox {d.getbbox()}  ({cmp})')
else:
    print(f'{a.name}: sizes differ {A.size} vs {B.size}  ({cmp})')
