#!/usr/bin/env python3
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
"""Run web-platform-tests (testharness.js tests) in OHB.

    wptrun.py WPT_ROOT DIR [DIR...] [-j N] [-k REGEX] [--json OUT.json] [--no-workers] [--real]

Starts tools/wpt/wptserve.py on 127.0.0.1:8123, loads each test in headless
OHB (with OHB_WPT=1, so testdriver input works) and waits for the harness to
finish. Prints pass/total per file, then the total. Set OHB=path/to/ohb to pick
the binary (default: target/release/ohb next to this checkout).

--real uses the checkout's own server instead (`./wpt serve`, started
separately, with `./wpt make-hosts-file` in /etc/hosts): Python handlers,
HTTPS on web-platform.test:8443 and the other origins work then. This mode
copies this directory's testdriver-vendor.js and a result reporter into the
checkout's resources/.
"""
import sys, os, subprocess, json, glob, time, concurrent.futures, re

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
args = sys.argv[1:]


def opt(name, default=None):
    if name in args:
        i = args.index(name)
        v = args[i + 1]
        del args[i:i + 2]
        return v
    return default


J = int(opt('-j', '3'))
filt = opt('-k')
out_json = opt('--json')
no_workers = '--no-workers' in args
if no_workers:
    args.remove('--no-workers')
real = '--real' in args
if real:
    args.remove('--real')
ROOT = os.path.abspath(args[0])
dirs = args[1:]
OHB = os.environ.get('OHB', os.path.join(REPO, 'target', 'release', 'ohb'))
PORT = int(os.environ.get('WPT_PORT', '8123'))


def globals_of(js):
    for line in open(js, errors='replace'):
        m = re.match(r'\s*//\s*META:\s*global=(.*)$', line)
        if m:
            return [g.strip() for g in m.group(1).split(',')]
    return ['window', 'dedicatedworker']


def variants_html(src):
    return re.findall(r'<meta\s+name=["\']?variant["\']?\s+content=["\']([^"\']*)["\']', src)


def variants_js(js):
    out = []
    for line in open(js, errors='replace'):
        m = re.match(r'\s*//\s*META:\s*variant=(.*)$', line)
        if m:
            out.append(m.group(1).strip())
    return out


def with_variants(rel, vs):
    return [rel + v for v in vs] if vs else [rel]


def collect(d):
    tests = []
    base = os.path.join(ROOT, d)
    files = [base] if os.path.isfile(base) else glob.glob(base + '/**/*', recursive=True)
    for f in sorted(files):
        rel = os.path.relpath(f, ROOT)
        bn = os.path.basename(f)
        if '/resources/' in '/' + rel or '/support/' in '/' + rel or ('tentative' in rel and not os.environ.get('WPT_TENTATIVE')) or '/reference/' in rel or '-ref.' in bn or '-ref-' in bn:
            continue
        if bn.endswith(('.html', '.htm', '.xhtml', '.svg')) and not bn.endswith(('.any.html', '.window.html', '.worker.html')):
            try:
                src = open(f, errors='replace').read()
            except Exception:
                continue
            if 'testharness.js' in src:
                tests += with_variants(rel, variants_html(src))
        elif bn.endswith('.any.js'):
            g = globals_of(f)
            vs = variants_js(f)
            if any(x in ('window', 'default') for x in g):
                tests += with_variants(rel[:-3] + '.html', vs)
            if not no_workers and any(x in ('worker', 'dedicatedworker', 'default') for x in g):
                tests += with_variants(rel[:-3] + '.worker.html', vs)
        elif bn.endswith('.window.js'):
            tests += with_variants(rel[:-3] + '.html', variants_js(f))
        elif bn.endswith('.worker.js') and not no_workers:
            tests += with_variants(rel[:-3] + '.html', variants_js(f))
    return tests


def run(rel):
    if real:
        secure = '.https.' in rel or '.serviceworker' in rel or '/service-workers/' in rel
        url = ('https://web-platform.test:8443/%s' if secure else 'http://web-platform.test:8000/%s') % rel
    else:
        url = 'http://127.0.0.1:%d/%s' % (PORT, rel)
    rel_q = rel
    rel = rel.split('?')[0].split('#')[0]
    long = False
    try:
        src = open(os.path.join(ROOT, re.sub(r'\.(any|window|worker)(\.worker)?\.html$', lambda m: '.' + m.group(1) + '.js', rel)), errors='replace').read() if rel.endswith(('.any.html', '.any.worker.html', '.window.html', '.worker.html')) else open(os.path.join(ROOT, rel), errors='replace').read()
        long = bool(re.search(r'name=["\']?timeout["\']?\s+content=["\']?long', src)) or 'META: timeout=long' in src
    except Exception:
        pass
    to = 70000 if long else 25000
    env = dict(os.environ, OHB_QUIET='1', OHB_WPT='1')
    if real:
        env['NO_PROXY'] = env.get('NO_PROXY', '') + ',web-platform.test,not-web-platform.test'
        env['no_proxy'] = env['NO_PROXY']
        env['SSL_CERT_FILE'] = CA_BUNDLE
    try:
        p = subprocess.run([OHB, '--screenshot', '/dev/null', '--width', '800', '--height', '600', '--wait', '100',
                            '--timeout', str(to), '--wait-for', '!!window.__wpt', '--eval', 'window.__wpt||"NORESULT"', url],
                           capture_output=True, text=True, timeout=to / 1000 + 30, env=env)
        # Only \n separates lines (splitlines() would also split on U+2028,
        # U+2029 and U+0085, which JSON leaves unescaped in test names).
        out = p.stdout.strip().split('\n')
    except subprocess.TimeoutExpired:
        return rel_q, 'TIMEOUT'
    last = [l for l in out if l.startswith('{') or l.startswith('"') or l == 'NORESULT']
    if not last:
        return rel_q, 'CRASH' if p.returncode else 'NOOUT'
    v = last[-1]
    try:
        v = json.loads(v)
        if isinstance(v, str):
            v = json.loads(v)
    except Exception:
        return rel_q, 'NORESULT'
    return rel_q, v


REPORT = """// OHB runner: results into window.__wpt (JSON). Like the browser vendors'
// automation reports, the HTML results table is turned off.
setup({ output: false });
add_completion_callback(function (tests, status) {
  var r = JSON.stringify({ status: status.status, message: status.message, tests: tests.map(function (t) { return [t.name, t.status, t.message]; }) });
  if (typeof window !== 'undefined') window.__wpt = r;
});
"""
CA_BUNDLE = os.path.join(HERE, '.wpt-ca-bundle.pem')
if real:
    import shutil
    shutil.copy(os.path.join(HERE, 'testdriver-vendor.js'), os.path.join(ROOT, 'resources', 'testdriver-vendor.js'))
    open(os.path.join(ROOT, 'resources', 'testharnessreport.js'), 'w').write(REPORT)
    sysca = next((p for p in ('/etc/ssl/certs/ca-certificates.crt', '/etc/pki/tls/certs/ca-bundle.crt', '/etc/ssl/cert.pem') if os.path.exists(p)), None)
    with open(CA_BUNDLE, 'w') as f:
        if sysca:
            f.write(open(sysca).read())
        f.write(open(os.path.join(ROOT, 'tools', 'certs', 'cacert.pem')).read())
    srv = None
else:
    srv = subprocess.Popen([sys.executable, os.path.join(HERE, 'wptserve.py'), ROOT, str(PORT)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.8)
tests = []
for d in dirs:
    tests += collect(d)
if filt:
    tests = [t for t in tests if re.search(filt, t)]
res = {}
progress = os.environ.get('WPT_PROGRESS')  # a file that gets each result as it lands


def note(rel, v):
    if not progress:
        return
    if isinstance(v, str):
        line = '%4d/%-4d %s  %s' % (0, 1, rel, v)
    else:
        t = v['tests']
        line = '%4d/%-4d %s' % (sum(1 for x in t if x[1] == 0), max(len(t), 1), rel)
    with open(progress, 'a') as f:
        f.write(line + '\n')


try:
    with concurrent.futures.ThreadPoolExecutor(J) as ex:
        futs = {ex.submit(run, t): t for t in tests}
        for fu in concurrent.futures.as_completed(futs):
            rel, v = fu.result()
            res[rel] = v
            note(rel, v)
    # Retry files that produced no result (load spikes), one at a time.
    for rel in [t for t in tests if isinstance(res.get(t), str)]:
        res[rel] = run(rel)[1]
finally:
    if srv:
        srv.kill()
tp = tt = 0
for rel in tests:
    v = res.get(rel)
    if isinstance(v, str) or v is None:
        print('%4d/%-4d %s  %s' % (0, 1, rel, v))
        tt += 1
        continue
    t = v['tests']
    p = sum(1 for x in t if x[1] == 0)
    n = max(len(t), 1)
    tp += p
    tt += n
    fails = ['%s: %s' % (x[0][:60], str(x[2])[:100]) for x in t if x[1] != 0][:3]
    harness = ('harness:' + str(v.get('message'))[:80] + ' ') if v.get('status') else ''
    print('%4d/%-4d %s  %s' % (p, n, rel, '' if p == n and not harness else harness + ' | '.join(fails)))
print('TOTAL %d/%d' % (tp, tt))
if out_json:
    json.dump(res, open(out_json, 'w'))
