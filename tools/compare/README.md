# Comparing OHB with Chromium

These scripts render the same page in Playwright's Chromium and in OHB and compare
screenshots or element boxes. Live sites change between loads and some proxies drop
large downloads, so both engines share a **web archive**: OHB's `OHB_ARCHIVE` mode
(`src/net/archive.rs`) and `pwarch.py` read and write the same files.

Requirements: `pip install playwright pillow` and a Chromium for Playwright; a built
OHB (`cargo build --release`, or point `OHB=` at another binary). Output goes to
`compare-out/` (override with `OHB_COMPARE_DIR`).

```
# 1. Record a site (Chromium first, then OHB fills anything extra it requests)
python3 tools/compare/compare.py hf https://huggingface.co/ --record

# 2. Compare offline from the archive: writes compare-out/shots/cmp_hf.png
python3 tools/compare/compare.py hf https://huggingface.co/

# 3. Find the first box that differs
python3 tools/compare/boxes.py hf https://huggingface.co/ 'h1, h1 ~ *' --extra "c.display+' '+c.lineHeight"

# Minimal repro pages (no archive)
python3 tools/compare/boxes.py - file:///path/repro.html '.a, .b'
```

The loop that worked well: look at the side-by-side, find the first element whose box
differs with `boxes.py`, reduce it to a few-line `file://` repro, fix the engine until
the boxes match, then re-run every site to check nothing else moved.

Useful OHB switches while doing this: `--eval 'js'`, `--dump-dom`, `OHB_INJECT=file.js`
(runs before page scripts), `OHB_CONSOLE=1`, `OHB_DEBUG_NET=1`, `OHB_PROFILE=1`,
`OHB_DUMP_LAYOUT=file`, `OHB_DUMP_DL=file`.
