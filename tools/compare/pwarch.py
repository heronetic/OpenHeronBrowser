# Copyright 2026 Marissa Tanner, Heronetic
# SPDX-License-Identifier: Apache-2.0

"""Web-archive routing for Playwright Chromium, in the same on-disk format as
OHB's OHB_ARCHIVE mode (see src/net/archive.rs), so both engines can render a
live site from identical bytes."""
import os, asyncio
# The same User-Agent OHB sends, so servers return the same documents.
UA='Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 OpenHeronBrowser/0.2.0'
# Hints Chromium acts on when replaying (Accept-CH restarts navigation, Link
# preloads/dictionaries stall it); they don't affect rendering.
DROP=('accept-ch','critical-ch','link','content-security-policy','content-security-policy-report-only','report-to','nel','strict-transport-security','alt-svc','origin-trial','reporting-endpoints','cross-origin-embedder-policy','cross-origin-opener-policy','permissions-policy','document-policy')
SKIP={'content-encoding','content-length','transfer-encoding','connection','keep-alive'}
def key(url):
    url=url.split('#')[0]
    h=0xcbf29ce484222325
    for b in url.encode():
        h^=b; h=(h*0x100000001b3)&0xFFFFFFFFFFFFFFFF
    return '%016x'%h
def lookup(d,url):
    k=key(url); m=os.path.join(d,k+'.meta')
    if not os.path.exists(m): return None
    lines=open(m,encoding='utf-8',errors='replace').read().split('\n')
    st=int(lines[0]); hdrs={}
    for l in lines[2:]:
        if ':' in l:
            a,b=l.split(':',1); a=a.strip().lower()
            hdrs[a]=(hdrs[a]+('\n' if a=='set-cookie' else ', ')+b.strip()) if a in hdrs else b.strip()
    body=open(os.path.join(d,k+'.body'),'rb').read() if os.path.exists(os.path.join(d,k+'.body')) else b''
    return st,hdrs,body
def store(d,url,st,headers,body):
    if 502<=st<=504: return
    k=key(url)
    meta=f'{st}\n{url.split("#")[0]}\n'
    for n,v in headers:
        if n.lower() in SKIP or '\n' in v: continue
        meta+=f'{n}: {v}\n'
    open(os.path.join(d,k+'.body'),'wb').write(body)
    open(os.path.join(d,k+'.meta'),'w').write(meta)
async def install(ctx, d, offline=False):
    """Serve GETs from archive dir `d`; record misses unless `offline`."""
    os.makedirs(d,exist_ok=True)
    async def handler(route):
        req=route.request
        if req.method!='GET' or not req.url.startswith('http'):
            return await route.continue_()
        hit=lookup(d,req.url)
        if hit is None and not offline:
            for attempt in range(3):
                try:
                    r=await route.fetch(max_redirects=0, timeout=45000)
                    body=await r.body()
                    hl=r.headers_array
                    if 502<=r.status<=504: raise Exception('5xx')
                    store(d,req.url,r.status,[(h['name'],h['value']) for h in hl],body)
                    hit=lookup(d,req.url); break
                except Exception as e:
                    await asyncio.sleep(0.4*(attempt+1))
        if hit is None:
            return await route.fulfill(status=404, body=b'not in archive')
        st,h,body=hit
        for x in DROP: h.pop(x,None)
        await route.fulfill(status=st, headers=h, body=body)
    await ctx.route('**/*', handler)
