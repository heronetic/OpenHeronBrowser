#!/usr/bin/env python3
# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0
"""A small stand-in for wptserve, enough to run web-platform-tests in OHB.

    wptserve.py WPT_ROOT [PORT]

Serves the checkout on 127.0.0.1:PORT (default 8123) and PORT+1 (a second
origin). Supports:
  * <file>.headers and `?pipe=status(N)` / `?pipe=header(Name,Value)`
  * `.sub.` substitution ({{host}}, {{ports[http][N]}}, {{domains[...]}},
    {{hosts[...][...]}}, {{location[...]}}, {{GET[...]}}, {{uuid()}})
  * generated wrappers for `.any.js`, `.window.js` and `.worker.js` tests
    (`X.any.html`, `X.any.worker.html`, `X.window.html`, `X.worker.html`)
  * this directory's testdriver-vendor.js in place of the checkout's, and a
    testharnessreport.js that publishes results as `window.__wpt`
"""
import http.server, os, re, sys, urllib.parse, mimetypes, uuid, threading

ROOT = os.path.abspath(sys.argv[1])
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8123
HERE = os.path.dirname(os.path.abspath(__file__))
HOST = '127.0.0.1'
ALT = 'localhost'  # a second host name for "cross-origin" domains

REPORT_JS = b"""// OHB runner: results into window.__wpt (JSON). Like the browser vendors'
// automation reports, the HTML results table is turned off.
setup({ output: false });
add_completion_callback(function (tests, status) {
  var r = JSON.stringify({ status: status.status, message: status.message, tests: tests.map(function (t) { return [t.name, t.status, t.message]; }) });
  if (typeof window !== 'undefined') window.__wpt = r;
});
"""


def meta_of(src):
    out = []
    for line in src.splitlines():
        m = re.match(r'\s*//\s*META:\s*(\w+)=(.*)$', line)
        if m:
            out.append((m.group(1), m.group(2).strip()))
        elif line.strip() and not line.strip().startswith('//'):
            break
    return out


def scripts_html(meta):
    s = ''
    for k, v in meta:
        if k == 'script':
            s += '<script src="%s"></script>\n' % v
    return s


def title_of(meta, default):
    for k, v in meta:
        if k == 'title':
            return v
    return default


def wrapper(path_js, kind):
    """HTML for a generated test page (kind: any, window, worker)."""
    src = open(path_js, errors='replace').read()
    meta = meta_of(src)
    base = os.path.basename(path_js)
    timeout = '<meta name="timeout" content="long">\n' if ('timeout', 'long') in meta else ''
    title = title_of(meta, base)
    head = '<!doctype html>\n<meta charset="utf-8">\n%s<title>%s</title>\n<script src="/resources/testharness.js"></script>\n<script src="/resources/testharnessreport.js"></script>\n' % (timeout, title)
    if kind == 'worker':
        return head + '<div id="log"></div>\n<script>fetch_tests_from_worker(new Worker("%s"));</script>\n' % (base[:-3] + ('.worker.js' if base.endswith('.any.js') else '.js') + '?wrapped=1')
    return head + scripts_html(meta) + '<div id="log"></div>\n<script src="%s"></script>\n' % base


def worker_js(path_js):
    src = open(path_js, errors='replace').read()
    meta = meta_of(src)
    base = os.path.basename(path_js)
    out = 'importScripts("/resources/testharness.js");\n'
    for k, v in meta:
        if k == 'script':
            out += 'importScripts("%s");\n' % v
    out += 'importScripts("%s");\ndone();\n' % base
    return out


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *a):
        pass

    def send_body(self, status, body, hdrs):
        self.send_response(status)
        for k, v in hdrs.items():
            self.send_header(k, v)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET()

    def do_POST(self):
        n = int(self.headers.get('Content-Length') or 0)
        self.post_body = self.rfile.read(n) if n else b''
        self.do_GET()

    def do_GET(self):
        u = urllib.parse.urlsplit(self.path)
        path = urllib.parse.unquote(u.path)
        query = urllib.parse.parse_qs(u.query or '', keep_blank_values=True)
        fp = os.path.normpath(os.path.join(ROOT, path.lstrip('/')))
        if not fp.startswith(ROOT):
            return self.send_body(403, b'forbidden', {'Content-Type': 'text/plain'})
        if os.path.isdir(fp):
            fp = os.path.join(fp, 'index.html')
        status = 200
        hdrs = {'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-cache'}
        body = None
        name = os.path.basename(fp)
        d = os.path.dirname(fp)
        if path == '/resources/testdriver-vendor.js':
            body = open(os.path.join(HERE, 'testdriver-vendor.js'), 'rb').read()
            hdrs['Content-Type'] = 'text/javascript'
        elif path == '/resources/testharnessreport.js':
            body = REPORT_JS
            hdrs['Content-Type'] = 'text/javascript'
        elif not os.path.isfile(fp):
            # Generated wrappers.
            for suffix, src_suffix, kind in (('.any.worker.html', '.any.js', 'worker'), ('.any.html', '.any.js', 'any'),
                                             ('.window.html', '.window.js', 'window'), ('.worker.html', '.worker.js', 'worker')):
                if name.endswith(suffix):
                    js = os.path.join(d, name[:-len(suffix)] + src_suffix)
                    if os.path.isfile(js):
                        body = wrapper(js, kind).encode()
                    break
            if body is None and name.endswith('.any.worker.js'):
                js = os.path.join(d, name[:-len('.any.worker.js')] + '.any.js')
                if os.path.isfile(js):
                    body = worker_js(js).encode()
                    hdrs['Content-Type'] = 'text/javascript'
            if body is None and name.endswith('.worker.js') and 'wrapped' in query:
                pass
            if body is None:
                return self.send_body(404, b'404', {'Content-Type': 'text/plain'})
        else:
            if name.endswith('.worker.js') and not name.endswith('.any.worker.js') and 'wrapped' in query:
                body = worker_js(fp).encode()
                hdrs['Content-Type'] = 'text/javascript'
            else:
                body = open(fp, 'rb').read()
                hdrs['Content-Type'] = mimetypes.guess_type(fp)[0] or 'application/octet-stream'
                if hdrs['Content-Type'] in ('text/html', 'text/javascript', 'application/javascript', 'text/css'):
                    hdrs['Content-Type'] += '; charset=utf-8' if fp.endswith(('.html', '.htm')) else ''
            if os.path.exists(fp + '.headers'):
                for line in open(fp + '.headers'):
                    if ':' in line:
                        k, v = line.split(':', 1)
                        hdrs[k.strip()] = v.strip()
            if '.sub.' in name or name.endswith('.sub'):
                body = self.substitute(body.decode('utf-8', 'replace'), query).encode()
        # Pipes.
        for p in (query.get('pipe') or []):
            for part in p.split('|'):
                m = re.match(r'status\((\d+)\)', part)
                if m:
                    status = int(m.group(1))
                m = re.match(r'header\(([^,]+),(.*)\)$', part)
                if m:
                    hdrs[m.group(1)] = m.group(2)
                if part == 'sub':
                    body = self.substitute(body.decode('utf-8', 'replace'), query).encode()
        self.send_body(status, body, hdrs)

    def substitute(self, text, query):
        port = self.server.server_address[1]

        def repl(m):
            e = m.group(1).strip()
            if e == 'host':
                return HOST
            if e == 'uuid()':
                return str(uuid.uuid4())
            m2 = re.match(r'ports\[(\w+)\]\[(\d+)\]', e)
            if m2:
                return str(PORT + int(m2.group(2)))
            m2 = re.match(r'domains\[(.*?)\]', e)
            if m2:
                return HOST if m2.group(1).strip('\'"') == '' else ALT
            m2 = re.match(r'hosts\[(.*?)\]\[(.*?)\]', e)
            if m2:
                return HOST if m2.group(1).strip('\'"') == '' and m2.group(2).strip('\'"') == '' else ALT
            m2 = re.match(r'GET\[(.*?)\]', e)
            if m2:
                return (query.get(m2.group(1)) or [''])[0]
            m2 = re.match(r'location\[(.*?)\]', e)
            if m2:
                k = m2.group(1)
                full = 'http://%s:%d%s' % (HOST, port, self.path)
                pu = urllib.parse.urlsplit(full)
                return {'host': '%s:%d' % (HOST, port), 'hostname': HOST, 'port': str(port), 'scheme': 'http',
                        'server': 'http://%s:%d' % (HOST, port), 'path': pu.path, 'query': ('?' + pu.query) if pu.query else '',
                        'pathname': pu.path, 'href': full}.get(k, '')
            m2 = re.match(r'headers\[(.*?)\]', e)
            if m2:
                return self.headers.get(m2.group(1).strip('\'"'), '')
            return m.group(0)
        return re.sub(r'\{\{(.*?)\}\}', repl, text)


def serve(port):
    http.server.ThreadingHTTPServer(('127.0.0.1', port), H).serve_forever()


if __name__ == '__main__':
    threading.Thread(target=serve, args=(PORT + 1,), daemon=True).start()
    serve(PORT)
