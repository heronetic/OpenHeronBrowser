// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Table layout (CSS 2.1 §17, css-tables-3): the automatic and fixed width
//! algorithms, colspan/rowspan, `<col>`/`<colgroup>`, the separated and
//! collapsing border models, baseline alignment in rows, and captions.

use super::*;
use crate::css::values::Color;
use std::collections::HashMap as Map;

/// Don't paint the box's own borders (collapsing model: the table paints them).
pub const TBL_NO_BORDER: u8 = 1;
/// Don't paint the box's decorations at all (rows, groups and columns paint
/// through their cells; empty cells with `empty-cells: hide`).
pub const TBL_NO_DECOR: u8 = 2;

/// One resolved border segment of a table in the collapsing model, relative to
/// the table's border box.
#[derive(Clone, Debug)]
pub struct CollapsedSeg {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub horizontal: bool,
    pub width: f32,
    pub style: BorderStyle,
    pub color: Color,
    rank: (u32, u8, u8),
}

struct RowInfo {
    row_box: Option<BoxId>,
    group: Option<BoxId>,
    cells: Vec<BoxId>,
}

struct CellPos {
    id: BoxId,
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
}

#[derive(Clone, Copy, Default)]
struct ColInfo {
    col: Option<BoxId>,
    group: Option<BoxId>,
}

struct Grid {
    rows: Vec<RowInfo>,
    captions: Vec<BoxId>,
    cells: Vec<CellPos>,
    ncols: usize,
    cols: Vec<ColInfo>,
}

impl Grid {
    fn nrows(&self) -> usize {
        self.rows.len()
    }
}

fn span_attr(l: &Layout, b: BoxId, name: &str) -> usize {
    l.boxes[b]
        .node
        .and_then(|n| l.doc.attr(n, name))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 1000)
}

fn collect_rows(l: &Layout, table: BoxId) -> (Vec<RowInfo>, Vec<BoxId>) {
    let mut head = Vec::new();
    let mut body = Vec::new();
    let mut foot = Vec::new();
    let mut captions = Vec::new();
    let mut loose: Vec<BoxId> = Vec::new();
    let flush_loose = |loose: &mut Vec<BoxId>, body: &mut Vec<RowInfo>, group: Option<BoxId>| {
        if !loose.is_empty() {
            body.push(RowInfo { row_box: None, group, cells: std::mem::take(loose) });
        }
    };
    let mut seen_head = false;
    let mut seen_foot = false;
    for &c in &l.boxes[table].children {
        let b = &l.boxes[c];
        if b.out_of_flow {
            continue;
        }
        match b.style.display {
            Display::TableCaption => captions.push(c),
            Display::TableColumn | Display::TableColumnGroup => {}
            Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => {
                flush_loose(&mut loose, &mut body, None);
                // Only the first header and footer group move; later ones stay in place.
                let target = match b.style.display {
                    Display::TableHeaderGroup if !seen_head => {
                        seen_head = true;
                        &mut head
                    }
                    Display::TableFooterGroup if !seen_foot => {
                        seen_foot = true;
                        &mut foot
                    }
                    _ => &mut body,
                };
                let mut gl: Vec<BoxId> = Vec::new();
                for &r in &b.children {
                    let rb = &l.boxes[r];
                    if rb.out_of_flow {
                        continue;
                    }
                    if rb.style.display == Display::TableRow {
                        if !gl.is_empty() {
                            target.push(RowInfo { row_box: None, group: Some(c), cells: std::mem::take(&mut gl) });
                        }
                        let cells: Vec<BoxId> = rb.children.iter().copied().filter(|&x| !l.boxes[x].out_of_flow).collect();
                        target.push(RowInfo { row_box: Some(r), group: Some(c), cells });
                    } else {
                        gl.push(r);
                    }
                }
                if !gl.is_empty() {
                    target.push(RowInfo { row_box: None, group: Some(c), cells: gl });
                }
            }
            Display::TableRow => {
                flush_loose(&mut loose, &mut body, None);
                let cells: Vec<BoxId> = b.children.iter().copied().filter(|&x| !l.boxes[x].out_of_flow).collect();
                body.push(RowInfo { row_box: Some(c), group: None, cells });
            }
            _ => loose.push(c),
        }
    }
    flush_loose(&mut loose, &mut body, None);
    let mut rows = head;
    rows.extend(body);
    rows.extend(foot);
    (rows, captions)
}

fn collect_cols(l: &Layout, table: BoxId) -> Vec<ColInfo> {
    let mut out = Vec::new();
    for &c in &l.boxes[table].children {
        match l.boxes[c].style.display {
            Display::TableColumnGroup => {
                let kids: Vec<BoxId> = l.boxes[c].children.iter().copied().filter(|&k| l.boxes[k].style.display == Display::TableColumn).collect();
                if kids.is_empty() {
                    for _ in 0..span_attr(l, c, "span") {
                        out.push(ColInfo { col: None, group: Some(c) });
                    }
                } else {
                    for k in kids {
                        for _ in 0..span_attr(l, k, "span") {
                            out.push(ColInfo { col: Some(k), group: Some(c) });
                        }
                    }
                }
            }
            Display::TableColumn => {
                for _ in 0..span_attr(l, c, "span") {
                    out.push(ColInfo { col: Some(c), group: None });
                }
            }
            _ => {}
        }
        if out.len() > 1000 {
            break;
        }
    }
    out
}

fn build_grid(l: &Layout, table: BoxId) -> Grid {
    let (rows, captions) = collect_rows(l, table);
    let mut cells = Vec::new();
    let mut occ: Vec<Vec<bool>> = Vec::new();
    let mut ncols = 0;
    for (ri, row) in rows.iter().enumerate() {
        while occ.len() <= ri {
            occ.push(Vec::new());
        }
        let mut col = 0;
        for &c in &row.cells {
            while occ[ri].get(col).copied().unwrap_or(false) {
                col += 1;
            }
            let colspan = span_attr(l, c, "colspan");
            // rowspan="0" spans to the end of the row group.
            let rowspan_raw = match l.boxes[c].node.and_then(|n| l.doc.attr(n, "rowspan")).map(|v| v.trim()) {
                Some("0") => rows.len(),
                _ => span_attr(l, c, "rowspan"),
            };
            let rowspan = rowspan_raw.min(rows.len() - ri).max(1);
            for r in ri..ri + rowspan {
                while occ.len() <= r {
                    occ.push(Vec::new());
                }
                if occ[r].len() < col + colspan {
                    occ[r].resize(col + colspan, false);
                }
                for x in col..col + colspan {
                    occ[r][x] = true;
                }
            }
            cells.push(CellPos { id: c, row: ri, col, colspan, rowspan });
            col += colspan;
            ncols = ncols.max(col);
        }
    }
    let mut cols = collect_cols(l, table);
    cols.resize(ncols, ColInfo::default());
    Grid { rows, captions, cells, ncols, cols }
}

/// Horizontal and vertical border spacing.
fn spacing(s: &ComputedStyle) -> (f32, f32) {
    if s.border_collapse {
        (0.0, 0.0)
    } else {
        (s.border_spacing, s.border_spacing_v)
    }
}

// ------------------------------------------------------------ collapsing borders

#[derive(Clone, Copy)]
struct Cand {
    w: f32,
    style: BorderStyle,
    color: Color,
    origin: u8,
}

#[derive(Clone, Copy, Default)]
struct Edge {
    w: f32,
    cand: Option<Cand>,
}

fn style_rank(s: BorderStyle) -> u8 {
    match s {
        BorderStyle::Double => 8,
        BorderStyle::Solid => 7,
        BorderStyle::Dashed => 6,
        BorderStyle::Dotted => 5,
        BorderStyle::Ridge => 4,
        BorderStyle::Outset => 3,
        BorderStyle::Groove => 2,
        BorderStyle::Inset => 1,
        _ => 0,
    }
}

/// Origins, strongest first: cell > row > row group > column > column group > table.
const O_CELL: u8 = 5;
const O_ROW: u8 = 4;
const O_GROUP: u8 = 3;
const O_COL: u8 = 2;
const O_COLGROUP: u8 = 1;
const O_TABLE: u8 = 0;

/// Accumulates the border candidates of one edge (CSS 2.1 §17.6.2.1).
#[derive(Default)]
struct Resolver {
    hidden: bool,
    best: Option<Cand>,
}

impl Resolver {
    fn add(&mut self, l: &Layout, b: Option<BoxId>, side: usize, origin: u8) {
        let Some(b) = b else { return };
        let s = &l.boxes[b].style;
        let st = s.border_style[side];
        if st == BorderStyle::Hidden {
            self.hidden = true;
            return;
        }
        if !st.visible() || s.border_width[side] <= 0.0 {
            return;
        }
        let c = Cand { w: s.border_width[side], style: st, color: s.border_color_at(side), origin };
        let wins = match &self.best {
            None => true,
            Some(b) => {
                if c.w != b.w {
                    c.w > b.w
                } else if style_rank(c.style) != style_rank(b.style) {
                    style_rank(c.style) > style_rank(b.style)
                } else {
                    // Same width and style: the origin decides; ties keep the
                    // earlier (left/top) candidate.
                    c.origin > b.origin
                }
            }
        };
        if wins {
            self.best = Some(c);
        }
    }
    fn edge(self) -> Edge {
        match (self.hidden, self.best) {
            (false, Some(c)) => Edge { w: c.w, cand: Some(c) },
            _ => Edge::default(),
        }
    }
}

struct Collapsed {
    /// Horizontal edges: (nrows + 1) × ncols.
    h: Vec<Vec<Edge>>,
    /// Vertical edges: nrows × (ncols + 1).
    v: Vec<Vec<Edge>>,
}

fn resolve_collapsed(l: &Layout, table: BoxId, g: &Grid) -> Collapsed {
    let nrows = g.nrows();
    let ncols = g.ncols;
    let mut at: Vec<Vec<Option<usize>>> = vec![vec![None; ncols]; nrows];
    for (i, c) in g.cells.iter().enumerate() {
        for r in c.row..(c.row + c.rowspan).min(nrows) {
            for x in c.col..(c.col + c.colspan).min(ncols) {
                at[r][x] = Some(i);
            }
        }
    }
    let row_box = |r: usize| g.rows[r].row_box;
    let group = |r: usize| g.rows[r].group;
    let group_first = |r: usize| r == 0 || g.rows[r - 1].group != g.rows[r].group;
    let group_last = |r: usize| r + 1 == nrows || g.rows[r + 1].group != g.rows[r].group;
    let col_first = |c: usize| c == 0 || g.cols[c - 1].col != g.cols[c].col;
    let col_last = |c: usize| c + 1 == ncols || g.cols[c + 1].col != g.cols[c].col;
    let cg_first = |c: usize| c == 0 || g.cols[c - 1].group != g.cols[c].group;
    let cg_last = |c: usize| c + 1 == ncols || g.cols[c + 1].group != g.cols[c].group;
    let mut h = vec![vec![Edge::default(); ncols]; nrows + 1];
    for (r, row) in h.iter_mut().enumerate() {
        for (c, e) in row.iter_mut().enumerate() {
            let above = if r > 0 { at[r - 1][c] } else { None };
            let below = if r < nrows { at[r][c] } else { None };
            if above.is_some() && above == below {
                continue; // inside a row-spanning cell
            }
            let mut res = Resolver::default();
            res.add(l, above.map(|i| g.cells[i].id), 2, O_CELL);
            res.add(l, below.map(|i| g.cells[i].id), 0, O_CELL);
            if r > 0 {
                res.add(l, row_box(r - 1), 2, O_ROW);
            }
            if r < nrows {
                res.add(l, row_box(r), 0, O_ROW);
            }
            if r > 0 && group_last(r - 1) {
                res.add(l, group(r - 1), 2, O_GROUP);
            }
            if r < nrows && group_first(r) {
                res.add(l, group(r), 0, O_GROUP);
            }
            if r == 0 || r == nrows {
                let side = if r == 0 { 0 } else { 2 };
                res.add(l, g.cols[c].col, side, O_COL);
                res.add(l, g.cols[c].group, side, O_COLGROUP);
                res.add(l, Some(table), side, O_TABLE);
            }
            *e = res.edge();
        }
    }
    let mut v = vec![vec![Edge::default(); ncols + 1]; nrows];
    for (r, row) in v.iter_mut().enumerate() {
        for (c, e) in row.iter_mut().enumerate() {
            let left = if c > 0 { at[r][c - 1] } else { None };
            let right = if c < ncols { at[r][c] } else { None };
            if left.is_some() && left == right {
                continue; // inside a column-spanning cell
            }
            let mut res = Resolver::default();
            res.add(l, left.map(|i| g.cells[i].id), 1, O_CELL);
            res.add(l, right.map(|i| g.cells[i].id), 3, O_CELL);
            if c == 0 || c == ncols {
                let side = if c == 0 { 3 } else { 1 };
                res.add(l, row_box(r), side, O_ROW);
                res.add(l, group(r), side, O_GROUP);
            }
            if c > 0 && col_last(c - 1) {
                res.add(l, g.cols[c - 1].col, 1, O_COL);
            }
            if c < ncols && col_first(c) {
                res.add(l, g.cols[c].col, 3, O_COL);
            }
            if c > 0 && cg_last(c - 1) {
                res.add(l, g.cols[c - 1].group, 1, O_COLGROUP);
            }
            if c < ncols && cg_first(c) {
                res.add(l, g.cols[c].group, 3, O_COLGROUP);
            }
            if c == 0 || c == ncols {
                res.add(l, Some(table), if c == 0 { 3 } else { 1 }, O_TABLE);
            }
            *e = res.edge();
        }
    }
    Collapsed { h, v }
}

fn set_override(l: &mut Layout, b: BoxId, o: Option<[f32; 4]>) {
    if l.boxes[b].border_override != o {
        l.boxes[b].border_override = o;
        l.boxes[b].intrinsic.set(None);
    }
}

/// Resolve the border model and the painting relationships (cell backgrounds,
/// collapsed border widths) before any sizing happens.
fn prepare(l: &mut Layout, table: BoxId, g: &Grid) -> Option<Collapsed> {
    let collapse = l.boxes[table].style.border_collapse;
    let nrows = g.nrows();
    // Rows, groups and columns paint their backgrounds through the cells.
    for r in &g.rows {
        for b in [r.row_box, r.group].into_iter().flatten() {
            l.boxes[b].tbl = TBL_NO_DECOR;
        }
    }
    for c in &g.cols {
        for b in [c.col, c.group].into_iter().flatten() {
            l.boxes[b].tbl = TBL_NO_DECOR;
        }
    }
    let hide_empty = !collapse;
    for c in &g.cells {
        let row = &g.rows[c.row];
        let ci = g.cols.get(c.col).copied().unwrap_or_default();
        let srcs = [ci.group, ci.col, row.group, row.row_box];
        l.boxes[c.id].cell_bgs = if srcs.iter().any(|s| s.is_some()) { Some(Rc::new(srcs)) } else { None };
        let empty = hide_empty && l.boxes[c.id].style.empty_cells_hide && is_empty_cell(l, c.id);
        l.boxes[c.id].tbl = if empty { TBL_NO_DECOR } else if collapse { TBL_NO_BORDER } else { 0 };
        if empty {
            l.boxes[c.id].cell_bgs = None;
        }
    }
    if !collapse {
        for c in &g.cells {
            set_override(l, c.id, None);
        }
        set_override(l, table, None);
        l.boxes[table].tbl = 0;
        return None;
    }
    let cb = resolve_collapsed(l, table, g);
    let ncols = g.ncols;
    for c in &g.cells {
        let ce = (c.col + c.colspan).min(ncols);
        let re = (c.row + c.rowspan).min(nrows);
        let top = (c.col..ce).map(|x| cb.h[c.row][x].w).fold(0.0, f32::max);
        let bottom = (c.col..ce).map(|x| cb.h[re][x].w).fold(0.0, f32::max);
        let left = (c.row..re).map(|y| cb.v[y][c.col].w).fold(0.0, f32::max);
        let right = (c.row..re).map(|y| cb.v[y][ce].w).fold(0.0, f32::max);
        set_override(l, c.id, Some([top / 2.0, right / 2.0, bottom / 2.0, left / 2.0]));
    }
    for r in &g.rows {
        for b in [r.row_box, r.group].into_iter().flatten() {
            l.boxes[b].tbl |= TBL_NO_BORDER;
        }
    }
    // The table's border is half of the widest outer edge on each side.
    let (tw, bw, lw, rw) = if nrows > 0 && ncols > 0 {
        (
            cb.h[0].iter().map(|e| e.w).fold(0.0, f32::max),
            cb.h[nrows].iter().map(|e| e.w).fold(0.0, f32::max),
            cb.v.iter().map(|r| r[0].w).fold(0.0, f32::max),
            cb.v.iter().map(|r| r[ncols].w).fold(0.0, f32::max),
        )
    } else {
        let s = &l.boxes[table].style;
        (s.bw(0) * 2.0, s.bw(2) * 2.0, s.bw(3) * 2.0, s.bw(1) * 2.0)
    };
    set_override(l, table, Some([tw / 2.0, rw / 2.0, bw / 2.0, lw / 2.0]));
    l.boxes[table].tbl = TBL_NO_BORDER;
    Some(cb)
}

fn is_empty_cell(l: &Layout, id: BoxId) -> bool {
    fn empty(l: &Layout, id: BoxId) -> bool {
        l.boxes[id].children.iter().all(|&c| {
            let b = &l.boxes[c];
            match &b.kind {
                BoxKind::Text(t) => t.chars().all(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}')),
                BoxKind::Block if b.pseudo == 3 => empty(l, c),
                _ => b.out_of_flow,
            }
        })
    }
    empty(l, id)
}

// ------------------------------------------------------------ column widths

#[derive(Clone, Copy, Default, Debug)]
struct ColC {
    min: f32,
    max: f32,
    pct: f32,
    fixed: bool,
}

fn column_constraints(l: &mut Layout, g: &Grid, table_w: Option<f32>, hsp: f32) -> Vec<ColC> {
    let ncols = g.ncols;
    let mut cols = vec![ColC::default(); ncols];
    // <col>/<colgroup> widths.
    for (i, ci) in g.cols.iter().enumerate() {
        let w = ci
            .col
            .map(|b| l.boxes[b].style.width.clone())
            .filter(|w| !w.is_auto())
            .or_else(|| ci.group.map(|b| l.boxes[b].style.width.clone()).filter(|w| !w.is_auto()));
        match w {
            Some(Len::Pct(p)) => cols[i].pct = cols[i].pct.max(p),
            Some(w) => {
                if let Some(px) = w.definite(None) {
                    cols[i].max = cols[i].max.max(px);
                    cols[i].fixed = true;
                }
            }
            None => {}
        }
    }
    let mut spans: Vec<(usize, usize, f32, f32, f32)> = Vec::new();
    for c in &g.cells {
        l.resolve_box_metrics(c.id, table_w.unwrap_or(0.0));
        let (mut mn, mut mx) = l.intrinsic_widths(c.id);
        let s = l.boxes[c.id].style.clone();
        let mut pct = 0.0;
        if let Len::Pct(p) = s.width {
            pct = p;
        } else if let Some(w) = l.specified_w(c.id, None) {
            mx = w.max(mn);
            if s.white_space == WhiteSpace::NoWrap {
                mn = mn.max(w);
            }
            if c.colspan == 1 {
                cols[c.col].fixed = true;
            }
        }
        if c.colspan == 1 {
            let k = &mut cols[c.col];
            k.min = k.min.max(mn);
            k.max = k.max.max(mx);
            k.pct = k.pct.max(pct);
        } else {
            spans.push((c.col, c.colspan, mn, mx, pct));
        }
    }
    // Spanning cells, narrowest spans first (Chromium TablesNG,
    // css-tables-3 §3.9.3): the cell's widths less the spacing between its
    // columns are distributed over them with the same guesses as the table
    // width; each column keeps the larger of its own and its share.
    spans.sort_by_key(|s| s.1);
    for (col, span, mn, mx, pct) in spans {
        let end = (col + span).min(ncols);
        let n = (end - col).max(1);
        let cell_min = mn - hsp * (n as f32 - 1.0);
        let cell_max = mx - hsp * (n as f32 - 1.0);
        let span_min: f32 = cols[col..end].iter().map(|c| c.min).sum();
        let span_max: f32 = cols[col..end].iter().map(|c| c.max).sum();
        if cell_min > span_min {
            let share = distribute_auto(&cols[col..end], cell_min);
            for (k, c) in (col..end).enumerate() {
                cols[c].min = cols[c].min.max(share[k]);
            }
        }
        if pct <= 0.0 && cell_max > span_max {
            let share = distribute_auto(&cols[col..end], cell_max);
            for (k, c) in (col..end).enumerate() {
                cols[c].max = cols[c].max.max(share[k]);
            }
        }
        if pct > 0.0 {
            let have_pct: f32 = cols[col..end].iter().map(|c| c.pct).sum();
            if pct > have_pct {
                let free: Vec<usize> = (col..end).filter(|&c| cols[c].pct == 0.0).collect();
                if !free.is_empty() {
                    let fm: f32 = free.iter().map(|&c| cols[c].max).sum();
                    for &c in &free {
                        let share = if fm > 0.0 { cols[c].max / fm } else { 1.0 / free.len() as f32 };
                        cols[c].pct = (pct - have_pct) * share;
                    }
                }
            }
        }
    }
    // Percentages may not exceed 100% in total.
    let mut total_pct: f32 = 0.0;
    for c in cols.iter_mut() {
        c.max = c.max.max(c.min);
        if c.pct > 0.0 {
            c.pct = c.pct.min((100.0 - total_pct).max(0.0));
            total_pct += c.pct;
        }
    }
    cols
}

/// css-tables-3 §3.9.3: distribute `target` over the columns using the
/// min-content, percentage, specified and max-content guesses.
fn distribute_auto(cols: &[ColC], target: f32) -> Vec<f32> {
    let n = cols.len();
    if n == 0 {
        return Vec::new();
    }
    let pct_w = |c: &ColC| c.min.max(c.pct / 100.0 * target);
    let g_min: Vec<f32> = cols.iter().map(|c| c.min).collect();
    let g_pct: Vec<f32> = cols.iter().map(|c| if c.pct > 0.0 { pct_w(c) } else { c.min }).collect();
    let g_spec: Vec<f32> = cols.iter().map(|c| if c.pct > 0.0 { pct_w(c) } else if c.fixed { c.max } else { c.min }).collect();
    let g_max: Vec<f32> = cols.iter().map(|c| if c.pct > 0.0 { pct_w(c) } else { c.max }).collect();
    let guesses = [g_min, g_pct, g_spec, g_max];
    let sums: Vec<f32> = guesses.iter().map(|g| g.iter().sum()).collect();
    if target <= sums[0] {
        return guesses[0].clone();
    }
    for i in 0..3 {
        if target <= sums[i + 1] {
            let span = sums[i + 1] - sums[i];
            let t = if span > 0.0 { (target - sums[i]) / span } else { 1.0 };
            return (0..n).map(|k| guesses[i][k] + (guesses[i + 1][k] - guesses[i][k]) * t).collect();
        }
    }
    // Wider than the max-content guess: grow auto columns, then fixed ones,
    // then percentage ones, then all.
    let mut w = guesses[3].clone();
    let extra = target - sums[3];
    let auto: Vec<usize> = (0..n).filter(|&k| cols[k].pct == 0.0 && !cols[k].fixed).collect();
    let fixed: Vec<usize> = (0..n).filter(|&k| cols[k].pct == 0.0 && cols[k].fixed).collect();
    let pct: Vec<usize> = (0..n).filter(|&k| cols[k].pct > 0.0).collect();
    let by = |w: &mut Vec<f32>, idx: &[usize], weight: &dyn Fn(usize) -> f32| -> bool {
        let total: f32 = idx.iter().map(|&k| weight(k)).sum();
        if idx.is_empty() || total <= 0.0 {
            return false;
        }
        for &k in idx {
            w[k] += extra * weight(k) / total;
        }
        true
    };
    let _ = by(&mut w, &auto, &|k| cols[k].max)
        || by(&mut w, &auto, &|_| 1.0)
        || by(&mut w, &fixed, &|k| cols[k].max)
        || by(&mut w, &pct, &|k| cols[k].pct)
        || by(&mut w, &(0..n).collect::<Vec<_>>(), &|_| 1.0);
    w
}

/// Intrinsic (min, max) of the column grid, including border spacing.
fn grid_intrinsic(cols: &[ColC], hsp: f32) -> (f32, f32) {
    let extra = hsp * (cols.len() + 1) as f32;
    let mn: f32 = cols.iter().map(|c| c.min).sum::<f32>() + extra;
    let mut mx: f32 = cols.iter().map(|c| c.max).sum::<f32>() + extra;
    // Percentage columns widen the max-content width so they can get their share.
    let total_pct: f32 = cols.iter().map(|c| c.pct).sum::<f32>().min(100.0);
    if total_pct > 0.0 {
        for c in cols.iter().filter(|c| c.pct > 0.0) {
            mx = mx.max(c.max * 100.0 / c.pct + extra);
        }
        let rest: f32 = cols.iter().filter(|c| c.pct == 0.0).map(|c| c.max).sum();
        if total_pct < 100.0 {
            mx = mx.max(rest * 100.0 / (100.0 - total_pct) + extra);
        } else if rest > 0.0 {
            mx = mx.max(1_000_000.0);
        }
    }
    (mn, mx)
}

pub fn table_intrinsic(l: &mut Layout, id: BoxId) -> (f32, f32) {
    let g = build_grid(l, id);
    prepare(l, id, &g);
    let (hsp, _) = spacing(&l.boxes[id].style);
    let fixed = l.boxes[id].style.table_layout_fixed && !l.boxes[id].style.width.is_auto();
    let cols = column_constraints(l, &g, None, hsp);
    let (mut mn, mut mx) = grid_intrinsic(&cols, hsp);
    if fixed {
        // Fixed layout ignores the content: only specified widths count.
        let spec: f32 = cols.iter().map(|c| if c.fixed { c.max } else { 0.0 }).sum::<f32>() + hsp * (cols.len() + 1) as f32;
        mn = spec;
        mx = mx.max(spec);
    }
    for &c in &g.captions {
        let (a, _) = l.intrinsic_widths(c);
        mn = mn.max(a);
        mx = mx.max(a);
    }
    (mn, mx.max(mn))
}

pub(super) fn shift_content(l: &mut Layout, id: BoxId, dy: f32) {
    if dy.abs() < 0.01 {
        return;
    }
    let kids = l.boxes[id].children.clone();
    for c in kids {
        if !l.boxes[c].out_of_flow {
            l.boxes[c].y += dy;
        }
    }
    if let Some(ifc) = l.boxes[id].ifc.as_mut() {
        for line in ifc.lines.iter_mut() {
            line.y += dy;
            for f in line.frags.iter_mut() {
                f.y += dy;
            }
        }
        for d in ifc.decos.iter_mut() {
            d.y += dy;
        }
    }
    if let Some(m) = l.boxes[id].marker.as_mut() {
        m.baseline += dy;
    }
}

/// Fixed table layout: widths from `<col>`s, then the first row's cells; the
/// rest is shared equally.
fn fixed_widths(l: &mut Layout, g: &Grid, avail: f32) -> Vec<f32> {
    let ncols = g.ncols;
    let mut widths = vec![0.0f32; ncols];
    let mut assigned = vec![false; ncols];
    for (i, ci) in g.cols.iter().enumerate() {
        let w = ci
            .col
            .map(|b| l.boxes[b].style.width.clone())
            .filter(|w| !w.is_auto())
            .or_else(|| ci.group.map(|b| l.boxes[b].style.width.clone()).filter(|w| !w.is_auto()));
        if let Some(px) = w.and_then(|w| w.definite(Some(avail))) {
            widths[i] = px.max(0.0);
            assigned[i] = true;
        }
    }
    for c in g.cells.iter().filter(|c| c.row == 0) {
        if (c.col..(c.col + c.colspan).min(ncols)).any(|k| assigned[k]) {
            continue;
        }
        l.resolve_box_metrics(c.id, avail);
        if let Some(cw) = l.specified_w(c.id, Some(avail)) {
            for k in c.col..(c.col + c.colspan).min(ncols) {
                widths[k] = cw / c.colspan as f32;
                assigned[k] = true;
            }
        }
    }
    let used: f32 = widths.iter().sum();
    let rest = assigned.iter().filter(|a| !**a).count();
    if rest > 0 {
        let each = ((avail - used) / rest as f32).max(0.0);
        for k in 0..ncols {
            if !assigned[k] {
                widths[k] = each;
            }
        }
    } else if used < avail && used > 0.0 {
        // Every column is specified: they grow proportionally.
        let k = avail / used;
        for w in widths.iter_mut() {
            *w *= k;
        }
    }
    widths
}

pub fn layout_table(l: &mut Layout, id: BoxId, cb_w: f32, cb_h: Option<f32>) {
    layout_table_inner(l, id, cb_w, cb_h);
    // Absolutely positioned children of the table and of its row groups and
    // rows (they aren't table parts): positioned from the table's content
    // box, laid out with the table's own list when it is their containing block.
    let (cx, cy) = (l.boxes[id].content_x(), l.boxes[id].content_y());
    let mut stack: Vec<BoxId> = l.boxes[id].children.clone();
    while let Some(c) = stack.pop() {
        if l.boxes[c].out_of_flow {
            l.register_oof(c, cx, cy);
        } else if matches!(l.boxes[c].style.display, Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup | Display::TableRow) {
            stack.extend(l.boxes[c].children.iter().copied());
        }
    }
    if l.boxes[id].style.is_abs_cb() {
        l.layout_abs_children(id);
    }
}

fn layout_table_inner(l: &mut Layout, id: BoxId, cb_w: f32, cb_h: Option<f32>) {
    let g = build_grid(l, id);
    let collapsed = prepare(l, id, &g);
    l.resolve_box_metrics(id, cb_w);
    let s = l.boxes[id].style.clone();
    let (hsp, vsp) = spacing(&s);
    let ncols = g.ncols;
    let nrows = g.nrows();
    let mut w = l.compute_block_width(id, cb_w);
    let pad = l.hpad(id);
    let fixed = s.table_layout_fixed && !s.width.is_auto();
    let cons = column_constraints(l, &g, Some(w), hsp);
    let (gmin, _) = grid_intrinsic(&cons, hsp);
    if !fixed && w < gmin + pad {
        w = gmin + pad;
    }
    let mut avail = (w - pad - hsp * (ncols + 1) as f32).max(0.0);
    let widths = if fixed {
        let ws = fixed_widths(l, &g, avail);
        let sum: f32 = ws.iter().sum();
        if sum > avail {
            w += sum - avail;
            avail = sum;
        }
        ws
    } else {
        distribute_auto(&cons, avail)
    };
    let _ = avail;
    l.boxes[id].w = w;
    let content_w = (w - pad).max(0.0);
    let mut col_x = vec![0.0f32; ncols + 1];
    {
        let mut x = hsp;
        for i in 0..ncols {
            col_x[i] = x;
            x += widths[i] + hsp;
        }
        col_x[ncols] = x;
    }
    // Top captions sit above the table's border and padding (in the table
    // wrapper box), as wide as the table's border box.
    let mut cap_top = 0.0;
    let mut bottom_caps = Vec::new();
    for &c in &g.captions {
        if l.boxes[c].style.caption_bottom {
            bottom_caps.push(c);
            continue;
        }
        l.resolve_box_metrics(c, w);
        l.layout_box(c, w, None, None, (0.0, 0.0));
        let b = &mut l.boxes[c];
        b.x = b.margin[3];
        b.y = cap_top + b.margin[0];
        cap_top += b.outer_h();
    }
    let cx = l.boxes[id].content_x();
    let cy = l.boxes[id].content_y() + cap_top;
    let y = 0.0;
    let table_top = y;

    // Lay out the cells at their column widths.
    struct CellM {
        h: f32,
        content_h: f32,
        baseline: f32,
        has_bl: bool,
        va: u8, // 0 baseline, 1 top, 2 middle, 3 bottom
    }
    let mut cm: Vec<CellM> = Vec::with_capacity(g.cells.len());
    for c in &g.cells {
        let end = (c.col + c.colspan).min(ncols);
        let cw = (col_x[end] - col_x[c.col] - hsp).max(0.0);
        // Lay out at the content height: the specified height only sets a
        // minimum for the row, it doesn't move the baseline.
        let orig = l.boxes[c.id].style.clone();
        if !orig.height.is_auto() {
            let mut st = (*orig).clone();
            st.height = Len::Auto;
            l.boxes[c.id].style = Rc::new(st);
        }
        l.layout_sized(c.id, Some(cw), None, cw, None);
        l.boxes[c.id].style = orig;
        let content_h = l.boxes[c.id].h;
        let mut h = content_h;
        if let Some(sh) = l.specified_h(c.id, None) {
            h = h.max(sh);
        }
        let b = &l.boxes[c.id];
        let real_bl = l.first_baseline(c.id);
        let baseline = real_bl.unwrap_or(b.h - b.border[2] - b.padding[2]);
        let va = match b.style.vertical_align {
            VerticalAlign::Top => 1,
            VerticalAlign::Middle => 2,
            VerticalAlign::Bottom => 3,
            _ => 0,
        };
        cm.push(CellM { h, content_h, baseline, has_bl: real_bl.is_some(), va });
    }
    // Row heights: specified row heights, single-row cells, baseline alignment.
    let mut row_h = vec![0.0f32; nrows];
    let mut row_fixed = vec![false; nrows];
    for (ri, r) in g.rows.iter().enumerate() {
        if let Some(rb) = r.row_box {
            if let Some(h) = l.boxes[rb].style.height.definite(None) {
                row_h[ri] = h;
                row_fixed[ri] = true;
            }
        }
    }
    let mut row_base = vec![f32::NEG_INFINITY; nrows];
    let mut row_desc = vec![0.0f32; nrows];
    for (c, m) in g.cells.iter().zip(&cm) {
        if m.va == 0 {
            row_base[c.row] = row_base[c.row].max(m.baseline);
            if c.rowspan == 1 {
                row_desc[c.row] = row_desc[c.row].max(m.content_h - m.baseline);
            }
        }
    }
    for (c, m) in g.cells.iter().zip(&cm) {
        if c.rowspan == 1 {
            row_h[c.row] = row_h[c.row].max(m.h);
        }
    }
    for r in 0..nrows {
        if row_base[r].is_finite() {
            row_h[r] = row_h[r].max(row_base[r] + row_desc[r]);
        }
    }
    // Row-spanning cells, shortest spans first: grow the spanned rows.
    let mut order: Vec<usize> = (0..g.cells.len()).filter(|&i| g.cells[i].rowspan > 1).collect();
    order.sort_by_key(|&i| g.cells[i].rowspan);
    for i in order {
        let c = &g.cells[i];
        let end = (c.row + c.rowspan).min(nrows);
        let have: f32 = row_h[c.row..end].iter().sum::<f32>() + vsp * (end - c.row - 1) as f32;
        let need = if cm[i].va == 0 && row_base[c.row].is_finite() { cm[i].h.max(cm[i].content_h + (row_base[c.row] - cm[i].baseline).max(0.0)) } else { cm[i].h };
        if need > have {
            let extra = need - have;
            let free: Vec<usize> = (c.row..end).filter(|&r| !row_fixed[r]).collect();
            let idx: Vec<usize> = if free.is_empty() { (c.row..end).collect() } else { free };
            let total: f32 = idx.iter().map(|&r| row_h[r]).sum();
            for &r in &idx {
                row_h[r] += if total > 0.0 { extra * row_h[r] / total } else { extra / idx.len() as f32 };
            }
        }
    }
    // Stretch rows to a specified table height.
    let spec_h = l.specified_h(id, cb_h);
    if let Some(th) = spec_h {
        let caps_bottom: f32 = 0.0;
        let inner = th - l.vpad(id) - table_top - caps_bottom - vsp * (nrows + 1) as f32;
        let sum: f32 = row_h.iter().sum();
        if inner > sum && nrows > 0 {
            let extra = inner - sum;
            for r in row_h.iter_mut() {
                *r += if sum > 0.0 { extra * *r / sum } else { extra / nrows as f32 };
            }
        }
    }
    let mut row_y = vec![0.0f32; nrows + 1];
    {
        let mut yy = table_top + vsp;
        for i in 0..nrows {
            row_y[i] = yy;
            yy += row_h[i] + vsp;
        }
        row_y[nrows] = yy;
    }
    // Position everything (table-relative content coordinates first).
    let mut tpos: Map<BoxId, (f32, f32)> = Map::new();
    tpos.insert(id, (-cx, -cy));
    let rows_top = row_y.first().copied().unwrap_or(table_top);
    let rows_bottom = if nrows > 0 { row_y[nrows] - vsp } else { table_top };
    // Column groups and columns.
    {
        let mut ext: Map<BoxId, (usize, usize)> = Map::new();
        for (i, ci) in g.cols.iter().enumerate() {
            for b in [ci.col, ci.group].into_iter().flatten() {
                let e = ext.entry(b).or_insert((i, i + 1));
                e.0 = e.0.min(i);
                e.1 = e.1.max(i + 1);
            }
        }
        let mut list: Vec<(BoxId, (usize, usize))> = ext.into_iter().collect();
        // Groups first so their columns can be placed relative to them.
        list.sort_by_key(|(b, _)| (l.boxes[*b].style.display != Display::TableColumnGroup, *b));
        for (b, (a, e)) in list {
            let (x0, x1) = (col_x[a], col_x[e] - hsp);
            tpos.insert(b, (x0, rows_top));
            let parent = l.boxes[b].parent.unwrap_or(id);
            let (px, py) = tpos.get(&parent).copied().unwrap_or((-cx, -cy));
            let bx = &mut l.boxes[b];
            bx.x = x0 - px;
            bx.y = rows_top - py;
            bx.w = (x1 - x0).max(0.0);
            bx.h = (rows_bottom - rows_top).max(0.0);
        }
    }
    let mut group_extent: Map<BoxId, (f32, f32)> = Map::new();
    for (ri, r) in g.rows.iter().enumerate() {
        if let Some(gb) = r.group {
            let e = group_extent.entry(gb).or_insert((row_y[ri], row_y[ri] + row_h[ri]));
            e.0 = e.0.min(row_y[ri]);
            e.1 = e.1.max(row_y[ri] + row_h[ri]);
        }
    }
    for (&gb, &(top, bottom)) in &group_extent {
        tpos.insert(gb, (hsp, top));
        let b = &mut l.boxes[gb];
        b.x = cx + hsp;
        b.y = cy + top;
        b.w = (content_w - 2.0 * hsp).max(0.0);
        b.h = bottom - top;
    }
    for (ri, r) in g.rows.iter().enumerate() {
        if let Some(rb) = r.row_box {
            tpos.insert(rb, (hsp, row_y[ri]));
            let (px, py) = match r.group.and_then(|gb| tpos.get(&gb)).copied() {
                Some(p) => p,
                None => (-cx, -cy),
            };
            let b = &mut l.boxes[rb];
            b.x = hsp - px;
            b.y = row_y[ri] - py;
            b.w = (content_w - 2.0 * hsp).max(0.0);
            b.h = row_h[ri];
        }
    }
    for (c, m) in g.cells.iter().zip(&cm) {
        let end_c = (c.col + c.colspan).min(ncols);
        let end_r = (c.row + c.rowspan).min(nrows);
        let x = col_x[c.col];
        let y0 = row_y[c.row];
        let cw = col_x[end_c] - col_x[c.col] - hsp;
        let ch = row_y[end_r] - row_y[c.row] - vsp;
        let content_h = m.content_h;
        let dy = match m.va {
            1 => 0.0,
            2 => (ch - content_h) / 2.0,
            3 => ch - content_h,
            _ => {
                if row_base[c.row].is_finite() {
                    row_base[c.row] - m.baseline
                } else {
                    0.0
                }
            }
        };
        shift_content(l, c.id, dy.max(0.0));
        let parent = l.boxes[c.id].parent.unwrap_or(id);
        let (px, py) = tpos.get(&parent).copied().unwrap_or((-cx, -cy));
        let b = &mut l.boxes[c.id];
        b.w = cw.max(0.0);
        b.h = ch.max(0.0);
        b.x = x - px;
        b.y = y0 - py;
    }
    let content_h = if nrows == 0 { table_top } else { row_y[nrows] };
    let mut grid_h = content_h.max(0.0) + l.vpad(id);
    if let Some(sh) = spec_h {
        grid_h = grid_h.max(sh);
    }
    // Bottom captions, below the table's bottom border.
    let mut cap_bottom = 0.0;
    for c in bottom_caps {
        l.resolve_box_metrics(c, w);
        l.layout_box(c, w, None, None, (0.0, 0.0));
        let b = &mut l.boxes[c];
        b.x = b.margin[3];
        b.y = cap_top + grid_h + cap_bottom + b.margin[0];
        cap_bottom += b.outer_h();
    }
    l.boxes[id].h = cap_top + grid_h + cap_bottom;
    l.boxes[id].caption_space = (cap_top, cap_bottom);
    // Collapsed border segments, relative to the table's border box.
    l.boxes[id].collapsed = collapsed.map(|cb| Rc::new(collapsed_segments(&cb, &col_x, &row_y, cx, cy)));
    l.finish_block_extras(id);
    l.boxes[id].baseline = if nrows > 0 && row_base[0].is_finite() {
        // The final cells: a baseline-aligned cell without a line box synthesizes
        // its baseline from its (stretched) content-box bottom, as TablesNG does.
        let fin = g
            .cells
            .iter()
            .zip(&cm)
            .filter(|(c, m)| c.row == 0 && m.va == 0)
            .map(|(c, m)| {
                let b = &l.boxes[c.id];
                if m.has_bl {
                    l.first_baseline(c.id).unwrap_or(row_base[0])
                } else {
                    b.h - b.border[2] - b.padding[2]
                }
            })
            .reduce(f32::max)
            .unwrap_or(row_base[0]);
        Some(cy + row_y[0] + fin)
    } else {
        // No baseline-aligned cell in the first row: like TablesNG, the row's
        // baseline is the lowest content-box bottom among its cells.
        g.cells
            .iter()
            .filter(|c| c.row == 0)
            .map(|c| {
                let b = &l.boxes[c.id];
                cy + row_y[c.row] + b.h - b.border[2] - b.padding[2]
            })
            .reduce(f32::max)
            .or_else(|| {
                g.cells.first().map(|c| {
                    let b = &l.boxes[c.id];
                    cy + row_y[c.row] + l.first_baseline(c.id).unwrap_or(b.h)
                })
            })
    };
}

fn collapsed_segments(cb: &Collapsed, col_x: &[f32], row_y: &[f32], cx: f32, cy: f32) -> Vec<CollapsedSeg> {
    let nrows = cb.v.len();
    let ncols = col_x.len().saturating_sub(1);
    let mut out = Vec::new();
    let vmax = |r: usize, c: usize| -> f32 {
        // Widest vertical edge meeting the joint at grid line (r, c).
        let a = if r > 0 { cb.v[r - 1][c].w } else { 0.0 };
        let b = if r < nrows { cb.v[r][c].w } else { 0.0 };
        a.max(b)
    };
    let hmax = |r: usize, c: usize| -> f32 {
        let a = if c > 0 { cb.h[r][c - 1].w } else { 0.0 };
        let b = if c < ncols { cb.h[r][c].w } else { 0.0 };
        a.max(b)
    };
    for r in 0..=nrows {
        for c in 0..ncols {
            let e = cb.h[r][c];
            let Some(k) = e.cand else { continue };
            let x0 = cx + col_x[c] - vmax(r, c) / 2.0;
            let x1 = cx + col_x[c + 1] + vmax(r, c + 1) / 2.0;
            let yc = cy + row_y[r];
            out.push(CollapsedSeg { x: x0, y: yc - e.w / 2.0, w: x1 - x0, h: e.w, horizontal: true, width: e.w, style: k.style, color: k.color, rank: ((e.w * 64.0) as u32, style_rank(k.style), k.origin) });
        }
    }
    for r in 0..nrows {
        for c in 0..=ncols {
            let e = cb.v[r][c];
            let Some(k) = e.cand else { continue };
            let y0 = cy + row_y[r] - hmax(r, c) / 2.0;
            let y1 = cy + row_y[r + 1] + hmax(r + 1, c) / 2.0;
            let xc = cx + col_x[c];
            out.push(CollapsedSeg { x: xc - e.w / 2.0, y: y0, w: e.w, h: y1 - y0, horizontal: false, width: e.w, style: k.style, color: k.color, rank: ((e.w * 64.0) as u32, style_rank(k.style), k.origin) });
        }
    }
    // Weaker borders first so the winners own the corners.
    out.sort_by_key(|s| s.rank);
    out
}
