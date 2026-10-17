# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0

"""Shared helpers for the comparison tools."""
import os, shutil
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, '..', '..'))
OUT = os.environ.get('OHB_COMPARE_DIR', os.path.join(REPO, 'compare-out'))
OHB = os.environ.get('OHB', os.path.join(REPO, 'target', 'release', 'ohb'))

def archive_dir(name):
    return os.path.join(OUT, 'arch', name)

def launch_kwargs(url):
    """Chromium launch args: use the environment's proxy for remote URLs."""
    kw = {'args': ['--ignore-certificate-errors']}
    px = os.environ.get('HTTPS_PROXY') or os.environ.get('https_proxy')
    if px and not url.startswith(('file:', 'http://127.', 'http://localhost')):
        kw['proxy'] = {'server': px, 'bypass': os.environ.get('NO_PROXY', '')}
    return kw
