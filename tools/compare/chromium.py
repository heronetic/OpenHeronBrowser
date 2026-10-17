#!/usr/bin/env python3
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
"""Chromium reference: screenshot a page and/or evaluate JS in it.

  chromium.py URL --shot OUT.png [--size 1280x900] [--wait MS] [--full] [--archive NAME [--offline]]
  chromium.py URL --eval 'JS expression' [...same options]
"""
import argparse, asyncio, json, sys
from playwright.async_api import async_playwright
import common, pwarch

async def run(a):
    w, h = map(int, a.size.split('x'))
    async with async_playwright() as p:
        b = await p.chromium.launch(**common.launch_kwargs(a.url))
        ctx = await b.new_context(viewport={'width': w, 'height': h}, ignore_https_errors=True,
                                  locale='en-US', timezone_id='UTC', user_agent=pwarch.UA)
        if a.archive:
            await pwarch.install(ctx, common.archive_dir(a.archive), a.offline)
        pg = await ctx.new_page()
        try:
            await pg.goto(a.url, wait_until='load', timeout=60000)
        except Exception as e:
            print('goto:', e, file=sys.stderr)
        await pg.wait_for_timeout(a.wait)
        if a.eval:
            r = await pg.evaluate(a.eval)
            print(r if isinstance(r, str) else json.dumps(r, indent=1))
        if a.shot:
            await pg.screenshot(path=a.shot, full_page=a.full)
        await b.close()

ap = argparse.ArgumentParser()
ap.add_argument('url'); ap.add_argument('--shot'); ap.add_argument('--eval')
ap.add_argument('--size', default='1280x900'); ap.add_argument('--wait', type=int, default=1500)
ap.add_argument('--full', action='store_true'); ap.add_argument('--archive'); ap.add_argument('--offline', action='store_true')
asyncio.run(run(ap.parse_args()))
