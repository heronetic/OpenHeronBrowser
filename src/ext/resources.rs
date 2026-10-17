// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Neutered stand-ins ("surrogates") served in place of blocked scripts that
//! pages depend on, so blocking a tracker doesn't break the page: queued
//! callbacks still run, and the APIs the page calls exist but do nothing.

/// A built-in resource by its `redirect=` name: (content type, body).
pub fn get(name: &str) -> Option<(&'static str, Vec<u8>)> {
    let js = |s: &str| Some(("application/javascript", s.as_bytes().to_vec()));
    match name.trim_start_matches("ohb-") {
        "noop.js" | "noopjs" | "noop-0.1s.js" => js("(function(){})();"),
        "noop.txt" | "nooptext" | "noop.json" => Some(("text/plain", Vec::new())),
        "noop.css" => Some(("text/css", Vec::new())),
        "noop.html" | "noopframe" => Some(("text/html", b"<!DOCTYPE html><html><head></head><body></body></html>".to_vec())),
        "1x1.gif" | "1x1-transparent.gif" | "2x2.png" | "2x2-transparent.png" | "32x32.png" | "32x32-transparent.png" | "3x2.png" | "3x2-transparent.png" => Some(("image/gif", GIF_1X1.to_vec())),
        "noop.mp4" | "noopmp4-1s" | "noop-0.5s.mp3" => Some(("video/mp4", Vec::new())),
        "google-analytics_analytics.js" | "google-analytics.com/analytics.js" | "google-analytics_ga.js" | "google-analytics.com/ga.js" => Some(("application/javascript", format!("{GA}{DATALAYER}}})();").into_bytes())),
        "googletagmanager_gtm.js" | "googletagmanager.com/gtm.js" => Some(("application/javascript", format!("{GTM}{DATALAYER}}})();").into_bytes())),
        "googletagservices_gpt.js" | "googletagservices.com/gpt.js" => js(GPT),
        "googlesyndication_adsbygoogle.js" | "googlesyndication.com/adsbygoogle.js" => js(ADSBYGOOGLE),
        "amazon_apstag.js" => js(APSTAG),
        "scorecardresearch_beacon.js" => js(COMSCORE),
        "fingerprint2.js" | "fingerprint3.js" => js(FINGERPRINT),
        "hd-main.js" | "noeval.js" => js("(function(){})();"),
        _ => None,
    }
}

const GIF_1X1: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

/// dataLayer handling shared by the Google tag stand-ins: run event
/// callbacks pages wait for (e.g. before following a link) and lift
/// anti-flicker "page hiding".
const DATALAYER: &str = r#"
  const dl = w.dataLayer;
  const runCb = (item) => {
    if (!item || typeof item !== 'object') return;
    if (typeof item.eventCallback === 'function') { const cb = item.eventCallback; item.eventCallback = () => {}; setTimeout(() => { try { cb(); } catch (e) {} }, 1); }
  };
  if (dl && typeof dl === 'object') {
    if (dl.hide && typeof dl.hide.end === 'function') { try { dl.hide.end(); } catch (e) {} dl.hide.end = () => {}; }
    if (typeof dl.push === 'function') {
      const push = dl.push;
      dl.push = function (...a) { for (const x of a) runCb(x); return push.apply(this, a); };
    }
    if (Array.isArray(dl)) for (const x of dl.slice()) runCb(x);
  }
  const hide = document.documentElement && document.documentElement.classList;
  if (hide) for (const c of ['async-hide']) hide.remove(c);
"#;

const GA: &str = concat!(
    "(function(){'use strict';const w=window;const noop=function(){};",
    "function Tracker(){} Tracker.prototype.get=noop;Tracker.prototype.set=noop;Tracker.prototype.send=noop;",
    "const name=w.GoogleAnalyticsObject||'ga';const queue=w[name];",
    "const ga=function(...a){const n=a.length;if(n===0)return;const last=a[n-1];",
    "let cb=null;if(last&&typeof last==='object'&&typeof last.hitCallback==='function')cb=last.hitCallback;",
    "else if(typeof last==='function')cb=()=>last(ga.create());",
    "else{const i=a.indexOf('hitCallback');if(i!==-1&&typeof a[i+1]==='function')cb=a[i+1];}",
    "if(cb){try{cb();}catch(e){}}};",
    "ga.create=function(){return new Tracker();};ga.getByName=function(){return new Tracker();};ga.getAll=function(){return[new Tracker()];};",
    "ga.remove=noop;ga.loaded=true;w[name]=ga;",
    "if(typeof queue==='function'&&Array.isArray(queue.q)){const q=queue.q.slice();queue.q.length=0;for(const e of q)ga(...e);}",
    "w._gaq=w._gaq||{push(){}};",
);

const GTM: &str = "(function(){'use strict';const w=window;w.ga=w.ga||function(){};w.google_tag_manager=w.google_tag_manager||{};";

const GPT: &str = r#"(function(){'use strict';
const w=window;const noop=function(){};const self=function(){return this;};
const slot={addService:self,clearCategoryExclusions:self,clearTargeting:self,defineSizeMapping:self,get:()=>null,getAdUnitPath:()=>'',getAttributeKeys:()=>[],getCategoryExclusions:()=>[],getDomId:()=>'',getResponseInformation:()=>null,getSlotElementId:()=>'',getSlotId(){return this;},getTargeting:()=>[],getTargetingKeys:()=>[],set:self,setCategoryExclusion:self,setClickUrl:self,setCollapseEmptyDiv:self,setConfig:self,setForceSafeFrame:self,setSafeFrameConfig:self,setTargeting:self,updateTargetingFromMap:self};
const mkSlot=(path,size,id)=>Object.assign(Object.create(slot),{getAdUnitPath:()=>path||'',getSlotElementId:()=>id||'',getDomId:()=>id||''});
const pubads={addEventListener:self,removeEventListener:self,clear:noop,clearCategoryExclusions:self,clearTagForChildDirectedTreatment:self,clearTargeting:self,collapseEmptyDivs:self,defineOutOfPagePassback:()=>mkSlot(),definePassback:()=>mkSlot(),disableInitialLoad:noop,display:noop,enableAsyncRendering:noop,enableLazyLoad:noop,enableSingleRequest:noop,enableSyncRendering:noop,enableVideoAds:noop,get:()=>null,getAttributeKeys:()=>[],getTargeting:()=>[],getTargetingKeys:()=>[],getSlots:()=>[],isInitialLoadDisabled:()=>true,refresh:noop,set:self,setCategoryExclusion:self,setCentering:noop,setCookieOptions:self,setForceSafeFrame:self,setLocation:self,setPrivacySettings:self,setPublisherProvidedId:self,setRequestNonPersonalizedAds:self,setSafeFrameConfig:self,setTagForChildDirectedTreatment:self,setTargeting:self,setVideoContent:self,updateCorrelator:noop};
const gt=w.googletag||{};const cmd=Array.isArray(gt.cmd)?gt.cmd:[];
Object.assign(gt,{apiReady:true,pubadsReady:true,companionAds:()=>({addEventListener:self,enableSyncLoading:noop,setRefreshUnfilledSlots:noop}),content:()=>({addEventListener:self,setContent:noop}),defineOutOfPageSlot:(p,id)=>mkSlot(p,null,id),defineSlot:(p,s,id)=>mkSlot(p,s,id),destroySlots:noop,disablePublisherConsole:noop,display:noop,enableServices:noop,getVersion:()=>'',pubads:()=>pubads,setAdIframeTitle:noop,setConfig:noop,sizeMapping:()=>({addSize(){return this;},build:()=>[]}),openConsole:noop,enums:{OutOfPageFormat:{}},secureSignalProviders:{push:noop},getConfig:()=>({})});
gt.cmd={push(...fs){for(const f of fs){try{if(typeof f==='function')f();}catch(e){}}return 0;},length:0};
w.googletag=gt;
for(const f of cmd){try{if(typeof f==='function')f();}catch(e){}}
})();"#;

const ADSBYGOOGLE: &str = r#"(function(){'use strict';
const w=window;const q=w.adsbygoogle;
w.adsbygoogle={loaded:true,push(){}};
if(Array.isArray(q)){}
for(const el of document.querySelectorAll('ins.adsbygoogle')){el.setAttribute('data-adsbygoogle-status','done');}
})();"#;

const APSTAG: &str = r#"(function(){'use strict';
const w=window;const noop=function(){};
const q=w.apstag&&w.apstag._Q||[];
w.apstag={_Q:[],fetchBids(cfg,cb){if(typeof cb==='function')setTimeout(()=>{try{cb([]);}catch(e){}},1);},init:noop,setDisplayBids:noop,targetingKeys:()=>[],rpa:noop,upa:noop,dpa:noop};
for(const e of q){if(e&&e[0]==='f'&&e[1])w.apstag.fetchBids(e[1][0],e[1][1]);}
})();"#;

const COMSCORE: &str = "(function(){'use strict';window.COMSCORE={purge(){return{};},beacon(){}};})();";

const FINGERPRINT: &str = r#"(function(){'use strict';
const w=window;const hash='0000000000000000';
function Fp(){} Fp.prototype.get=function(cb){setTimeout(()=>{try{cb(hash,[]);}catch(e){}},1);};
Fp.get=function(o,cb){if(typeof o==='function')cb=o;setTimeout(()=>{try{cb([]);}catch(e){}},1);};
Fp.getPromise=()=>Promise.resolve([]);Fp.getV18=Fp.get;Fp.x64hash128=()=>hash;Fp.load=()=>Promise.resolve({get:()=>Promise.resolve({visitorId:hash,components:{}})});
w.Fingerprint2=Fp;w.FingerprintJS=Fp;
})();"#;
