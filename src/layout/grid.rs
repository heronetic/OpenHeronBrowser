// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Grid Layout (explicit/implicit tracks, fr units, repeat(), minmax(),
//! named areas, auto-placement).

use super::*;
use std::collections::HashMap as Map;

#[derive(Clone, Debug)]
enum T {
    Fixed(f32),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    MinMax(Box<T>, Box<T>),
    Fit(f32),
}

fn conv(t: &Track, basis: Option<f32>) -> T {
    match t {
        Track::Len(l) => match l.definite(basis) {
            Some(v) => T::Fixed(v.max(0.0)),
            None => T::Auto,
        },
        Track::Fr(f) => T::Fr(*f),
        Track::Auto => T::Auto,
        Track::MinContent => T::MinContent,
        Track::MaxContent => T::MaxContent,
        Track::MinMax(a, b) => T::MinMax(Box::new(conv(a, basis)), Box::new(conv(b, basis))),
        Track::FitContent(l) => T::Fit(l.resolve(basis.unwrap_or(0.0))),
        Track::Repeat(_, _) | Track::Names(_) => T::Auto,
    }
}

fn fixed_min(t: &T) -> Option<f32> {
    match t {
        T::Fixed(v) => Some(*v),
        T::MinMax(a, b) => fixed_min(a).or_else(|| fixed_min(b)),
        T::Fit(v) => Some(*v),
        _ => None,
    }
}

/// Whether a track's minimum sizing function is intrinsic (content can grow it).
fn intrinsic_min(t: &T) -> bool {
    match t {
        T::Fixed(_) => false,
        T::MinMax(a, _) => !matches!(**a, T::Fixed(_)),
        _ => true,
    }
}

/// Whether a track's minimum is `auto`, which uses the items' minimum contribution.
fn auto_min(t: &T) -> bool {
    match t {
        T::Auto | T::Fr(_) | T::Fit(_) => true,
        T::MinMax(a, _) => matches!(**a, T::Auto),
        _ => false,
    }
}

/// Whether a track has a flexible (`fr`) maximum.
fn flexible(t: &T) -> bool {
    match t {
        T::Fr(_) => true,
        T::MinMax(_, b) => matches!(**b, T::Fr(_)),
        _ => false,
    }
}

type LineNames = Map<String, Vec<usize>>;

fn expand(list: Option<&Rc<Vec<Track>>>, basis: Option<f32>, gap: f32) -> (Vec<T>, bool, LineNames) {
    let mut out = Vec::new();
    let mut names: LineNames = Map::new();
    let mut auto_fit = false;
    let add = |names: &mut LineNames, ns: &[String], at: usize| {
        for n in ns {
            let v = names.entry(n.clone()).or_default();
            if !v.contains(&at) {
                v.push(at);
            }
        }
    };
    if let Some(list) = list {
        for t in list.iter() {
            match t {
                Track::Names(ns) => add(&mut names, ns, out.len()),
                Track::Repeat(cnt, tracks) => {
                    let conv_tracks: Vec<T> = tracks.iter().filter(|x| !matches!(x, Track::Names(_))).map(|x| conv(x, basis)).collect();
                    let n = match cnt {
                        RepeatCount::Count(n) => *n as usize,
                        RepeatCount::AutoFill | RepeatCount::AutoFit => {
                            if matches!(cnt, RepeatCount::AutoFit) {
                                auto_fit = true;
                            }
                            let per: f32 = conv_tracks.iter().map(|t| fixed_min(t).unwrap_or(0.0)).sum::<f32>();
                            match basis {
                                Some(b) if per > 0.0 => {
                                    let k = conv_tracks.len() as f32;
                                    (((b + gap) / (per + gap * k)).floor() as usize).max(1)
                                }
                                _ => 1,
                            }
                        }
                    };
                    for _ in 0..n.min(1000) {
                        for x in tracks.iter() {
                            match x {
                                Track::Names(ns) => add(&mut names, ns, out.len()),
                                other => out.push(conv(other, basis)),
                            }
                        }
                    }
                }
                other => out.push(conv(other, basis)),
            }
        }
    }
    for v in names.values_mut() {
        v.sort_unstable();
    }
    (out, auto_fit, names)
}

struct GItem {
    id: BoxId,
    col: usize,
    col_span: usize,
    row: usize,
    row_span: usize,
}

fn line_index(l: &GridLine, count: usize) -> Option<usize> {
    match l {
        GridLine::Line(n) if *n > 0 => Some((*n - 1) as usize),
        GridLine::Line(n) if *n < 0 => Some(((count as i32 + 1) + *n).max(0) as usize),
        _ => None,
    }
}

fn resolve_axis(start: &GridLine, end: &GridLine, count: usize, areas: &Map<String, (usize, usize)>, names: &LineNames) -> (Option<usize>, usize) {
    // Named area lookup.
    if let GridLine::Name(n) = start {
        if let Some(&(a, b)) = areas.get(&**n) {
            if let GridLine::Auto = end {
                return (Some(a), (b - a).max(1));
            }
        }
    }
    // A named line: explicit `[name]` lines first, then the implicit `-start`/`-end`
    // lines of named areas and of line pairs `name-start`/`name-end`.
    let named = |n: &str, is_start: bool| -> Option<usize> {
        let suffixed = format!("{n}{}", if is_start { "-start" } else { "-end" });
        if let Some(v) = names.get(&suffixed) {
            return v.first().copied();
        }
        if let Some(&(a, b)) = areas.get(n) {
            return Some(if is_start { a } else { b });
        }
        if let Some(v) = names.get(n) {
            return v.first().copied();
        }
        if let Some(stripped) = n.strip_suffix(if is_start { "-start" } else { "-end" }) {
            if let Some(&(a, b)) = areas.get(stripped) {
                return Some(if is_start { a } else { b });
            }
        }
        None
    };
    let s = match start {
        GridLine::Name(n) => named(n, true),
        other => line_index(other, count),
    };
    let mut e = match end {
        GridLine::Name(n) => named(n, false),
        other => line_index(other, count),
    };
    // `grid-column: name` with an auto end and a start found via `name-start`
    // places the item up to `name-end`.
    if e.is_none() && matches!(end, GridLine::Auto) {
        if let GridLine::Name(n) = start {
            if names.contains_key(&format!("{n}-start")) || names.contains_key(&format!("{n}-end")) {
                e = named(n, false);
            }
        }
    }
    match (s, e, start, end) {
        (Some(s), Some(e), _, _) if e > s => (Some(s), e - s),
        (Some(s), Some(e), _, _) if e < s => (Some(e), s - e),
        (Some(s), _, _, GridLine::Span(n)) => (Some(s), *n as usize),
        (Some(s), _, _, _) => (Some(s), 1),
        (None, Some(e), GridLine::Span(n), _) => (Some(e.saturating_sub(*n as usize)), *n as usize),
        (None, Some(e), _, _) => (Some(e.saturating_sub(1)), 1),
        (None, None, GridLine::Span(n), _) => (None, *n as usize),
        (None, None, _, GridLine::Span(n)) => (None, *n as usize),
        _ => (None, 1),
    }
}

/// Min-content and max-content widths of a grid container's content: items
/// are assigned to explicit (or auto-placed) columns, each column takes its
/// fixed size or its items' contributions, and gaps are added.
pub fn grid_intrinsic(l: &mut Layout, id: BoxId) -> (f32, f32) {
    let s = l.boxes[id].style.clone();
    let gap = s.column_gap.definite(None).unwrap_or(0.0).max(0.0);
    let (mut cols, auto_fit, names) = expand(s.grid_template_columns.as_ref(), None, gap);
    if let Some(areas) = &s.grid_template_areas {
        let n = areas.iter().map(|r| r.len()).max().unwrap_or(0);
        while cols.len() < n {
            cols.push(T::Auto);
        }
    }
    let explicit = cols.len();
    let auto_col: Vec<T> = s.grid_auto_columns.as_ref().map(|v| v.iter().map(|t| conv(t, None)).collect()).unwrap_or_else(|| vec![T::Auto]);
    let kids: Vec<BoxId> = l.boxes[id].children.iter().copied().filter(|&c| !l.boxes[c].out_of_flow).collect();
    let mut areas: Map<String, (usize, usize)> = Map::new();
    if let Some(ta) = &s.grid_template_areas {
        for rowv in ta.iter() {
            for (c, name) in rowv.iter().enumerate() {
                if name.chars().all(|ch| ch == '.') {
                    continue;
                }
                let e = areas.entry(name.clone()).or_insert((c, c + 1));
                e.0 = e.0.min(c);
                e.1 = e.1.max(c + 1);
            }
        }
    }
    // (column, span, min, max) per item.
    let mut items: Vec<(usize, usize, f32, f32)> = Vec::new();
    let mut cursor = 0usize;
    let column_flow = s.grid_auto_flow_column;
    let ncols_base = explicit.max(1);
    for c in kids {
        let cs = l.boxes[c].style.clone();
        let (start, span) = resolve_axis(&cs.grid_column.0, &cs.grid_column.1, ncols_base, &areas, &names);
        let span = span.max(1);
        let col = match start {
            Some(c0) => c0,
            None if column_flow => {
                // Column flow: each item opens a new column unless rows are explicit.
                let c0 = cursor;
                cursor += span;
                c0
            }
            None => {
                if cursor + span > ncols_base {
                    cursor = 0;
                }
                let c0 = cursor;
                cursor += span;
                c0
            }
        };
        let (mn, mx) = l.intrinsic_widths(c);
        let m = l.boxes[c].margin[1] + l.boxes[c].margin[3];
        items.push((col, span, mn + m, mx + m));
    }
    let ncols = items.iter().map(|i| i.0 + i.1).max().unwrap_or(0).max(explicit);
    while cols.len() < ncols {
        let i = cols.len() - explicit.min(cols.len());
        cols.push(auto_col[i % auto_col.len()].clone());
    }
    let mut mins = vec![0.0f32; cols.len()];
    let mut maxs = vec![0.0f32; cols.len()];
    let mut used = vec![false; cols.len()];
    for &(c, _, mn, mx) in items.iter().filter(|i| i.1 == 1) {
        if c < cols.len() {
            mins[c] = mins[c].max(mn);
            maxs[c] = maxs[c].max(mx);
            used[c] = true;
        }
    }
    let mut tot_min = 0.0;
    let mut tot_max = 0.0;
    let mut counted = 0usize;
    let mut ta = vec![0.0f32; cols.len()];
    let mut tb = vec![0.0f32; cols.len()];
    for (i, t) in cols.iter().enumerate() {
        if auto_fit && !used[i] && !items.iter().any(|it| i >= it.0 && i < it.0 + it.1) {
            continue;
        }
        let (a, b) = match t {
            T::Fixed(v) => (*v, *v),
            T::MinMax(lo, hi) => {
                let lo_v = fixed_min(lo).unwrap_or(mins[i]);
                let hi_v = match &**hi {
                    T::Fixed(v) => *v,
                    T::MinContent => mins[i],
                    _ => maxs[i],
                };
                (lo_v, hi_v.max(lo_v))
            }
            T::MinContent => (mins[i], mins[i]),
            T::Fit(lim) => (mins[i], maxs[i].min(*lim).max(mins[i])),
            _ => (mins[i], maxs[i]),
        };
        ta[i] = a;
        tb[i] = b;
        tot_min += a;
        tot_max += b;
        counted += 1;
    }
    // Flexible tracks share one flex fraction when the free space is
    // indefinite (css-grid §12.7.1): `1fr 1fr` with items of different
    // widths is as wide as twice the larger one.
    {
        let fr_of = |t: &T| match t {
            T::Fr(f) => Some(*f),
            T::MinMax(_, b) => match **b {
                T::Fr(f) => Some(f),
                _ => None,
            },
            _ => None,
        };
        let live = |i: usize| !(auto_fit && !used[i] && !items.iter().any(|it| i >= it.0 && i < it.0 + it.1));
        let flex: Vec<(usize, f32)> = cols.iter().enumerate().filter_map(|(i, t)| fr_of(t).filter(|_| live(i)).map(|f| (i, f.max(1.0)))).collect();
        if flex.len() > 1 {
            let frac = flex.iter().map(|&(i, f)| tb[i] / f).fold(0.0f32, f32::max);
            for &(i, f) in &flex {
                let want = (frac * f).max(tb[i]);
                tot_max += want - tb[i];
                tb[i] = want;
            }
        }
    }
    // Items spanning several columns grow only the tracks with an intrinsic
    // minimum (min-content size) or a non-fixed maximum (max-content size).
    let intr_min = |t: &T| match t {
        T::Fixed(_) => false,
        T::MinMax(lo, _) => fixed_min(lo).is_none(),
        _ => true,
    };
    let intr_max = |t: &T| match t {
        T::Fixed(_) => false,
        T::MinMax(_, hi) => !matches!(**hi, T::Fixed(_)),
        _ => true,
    };
    for &(c, span, mn, mx) in items.iter().filter(|i| i.1 > 1) {
        let end = (c + span).min(cols.len());
        if c >= end {
            continue;
        }
        let inner_gap = gap * (span.saturating_sub(1)) as f32;
        let have_min: f32 = (c..end).map(|i| ta[i]).sum::<f32>() + inner_gap;
        let have_max: f32 = (c..end).map(|i| tb[i]).sum::<f32>() + inner_gap;
        let span_t = &cols[c..end];
        // An item spanning a flexible track has no automatic minimum
        // (css-grid-1 §6.6): it doesn't widen the min-content size.
        if !span_t.iter().any(flexible) && span_t.iter().any(intr_min) && mn > have_min {
            let add = mn - have_min;
            tot_min += add;
            let k = (c..end).find(|&i| intr_min(&cols[i])).unwrap();
            ta[k] += add;
            if tb[k] < ta[k] {
                tot_max += ta[k] - tb[k];
                tb[k] = ta[k];
            }
        }
        let have_max = have_max.max((c..end).map(|i| tb[i]).sum::<f32>() + inner_gap);
        if span_t.iter().any(intr_max) && mx > have_max {
            tot_max += mx - have_max;
            let k = (c..end).find(|&i| intr_max(&cols[i])).unwrap();
            tb[k] += mx - have_max;
        }
    }
    let gaps = gap * counted.saturating_sub(1) as f32;
    (tot_min + gaps, (tot_max + gaps).max(tot_min + gaps))
}

pub fn layout_grid(l: &mut Layout, id: BoxId, cb_w: f32, cb_h: Option<f32>) {
    l.resolve_box_metrics(id, cb_w);
    let w = l.compute_block_width(id, cb_w);
    l.boxes[id].w = w;
    let s = l.boxes[id].style.clone();
    let content_w = (w - l.hpad(id)).max(0.0);
    let spec_h = l.specified_h(id, cb_h);
    let content_h_def = spec_h.map(|h| (l.clamp_h(id, h, cb_h) - l.vpad(id)).max(0.0));
    let cx = l.boxes[id].content_x();
    let cy = l.boxes[id].content_y();
    let col_gap = s.column_gap.definite(Some(content_w)).unwrap_or(0.0).max(0.0);
    let row_gap = s.row_gap.definite(content_h_def).unwrap_or(0.0).max(0.0);

    let (mut cols, auto_fit, col_names) = expand(s.grid_template_columns.as_ref(), Some(content_w), col_gap);
    let (mut rows, _, row_names) = expand(s.grid_template_rows.as_ref(), content_h_def, row_gap);
    let auto_col: Vec<T> = s.grid_auto_columns.as_ref().map(|v| v.iter().map(|t| conv(t, Some(content_w))).collect()).unwrap_or_else(|| vec![T::Auto]);
    let auto_row: Vec<T> = s.grid_auto_rows.as_ref().map(|v| v.iter().map(|t| conv(t, content_h_def)).collect()).unwrap_or_else(|| vec![T::Auto]);

    // Named areas.
    let mut col_areas: Map<String, (usize, usize)> = Map::new();
    let mut row_areas: Map<String, (usize, usize)> = Map::new();
    if let Some(areas) = &s.grid_template_areas {
        for (r, rowv) in areas.iter().enumerate() {
            for (c, name) in rowv.iter().enumerate() {
                if name == "." || name.chars().all(|ch| ch == '.') {
                    continue;
                }
                let e = col_areas.entry(name.clone()).or_insert((c, c + 1));
                e.0 = e.0.min(c);
                e.1 = e.1.max(c + 1);
                let e = row_areas.entry(name.clone()).or_insert((r, r + 1));
                e.0 = e.0.min(r);
                e.1 = e.1.max(r + 1);
            }
        }
        let ncols = areas.iter().map(|r| r.len()).max().unwrap_or(0);
        while cols.len() < ncols {
            cols.push(T::Auto);
        }
        while rows.len() < areas.len() {
            rows.push(T::Auto);
        }
    }

    let mut kids: Vec<BoxId> = Vec::new();
    for &c in &l.boxes[id].children.clone() {
        if l.boxes[c].out_of_flow {
            l.register_oof(c, cx, cy);
        } else {
            kids.push(c);
        }
    }
    kids.sort_by_key(|&c| l.boxes[c].style.order);

    // Placement.
    let explicit_cols = cols.len().max(1);
    let mut placed: Vec<GItem> = Vec::new();
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    let mut ncols = explicit_cols;
    // Make sure rows 0..rows exist (callers pass `row + span`).
    let ensure = |occ: &mut Vec<Vec<bool>>, rows: usize, ncols: usize| {
        while occ.len() < rows {
            occ.push(vec![false; ncols]);
        }
        for row in occ.iter_mut() {
            if row.len() < ncols {
                row.resize(ncols, false);
            }
        }
    };
    let mut pending: Vec<(BoxId, Option<usize>, usize, Option<usize>, usize)> = Vec::new();
    for &c in &kids {
        let cs = l.boxes[c].style.clone();
        let (cstart, cspan) = resolve_axis(&cs.grid_column.0, &cs.grid_column.1, explicit_cols, &col_areas, &col_names);
        let (rstart, rspan) = resolve_axis(&cs.grid_row.0, &cs.grid_row.1, rows.len().max(1), &row_areas, &row_names);
        if let Some(cs0) = cstart {
            ncols = ncols.max(cs0 + cspan);
        } else {
            ncols = ncols.max(cspan);
        }
        pending.push((c, cstart, cspan.max(1), rstart, rspan.max(1)));
    }
    let column_flow = s.grid_auto_flow_column;
    // Pass 1: fully explicit items.
    for &(c, cs0, cspan, rs0, rspan) in &pending {
        if let (Some(c0), Some(r0)) = (cs0, rs0) {
            ensure(&mut occupied, r0 + rspan, ncols);
            for r in r0..r0 + rspan {
                for cc in c0..(c0 + cspan).min(ncols) {
                    occupied[r][cc] = true;
                }
            }
            placed.push(GItem { id: c, col: c0, col_span: cspan, row: r0, row_span: rspan });
        }
    }
    // Pass 2: auto-placement.
    let (mut cur_r, mut cur_c) = (0usize, 0usize);
    for &(c, cs0, cspan, rs0, rspan) in &pending {
        if cs0.is_some() && rs0.is_some() {
            continue;
        }
        let cspan = cspan.min(ncols);
        let fits = |occ: &Vec<Vec<bool>>, r: usize, c0: usize, ncols: usize| -> bool {
            if c0 + cspan > ncols {
                return false;
            }
            for rr in r..r + rspan {
                if let Some(row) = occ.get(rr) {
                    for cc in c0..c0 + cspan {
                        if row.get(cc).copied().unwrap_or(false) {
                            return false;
                        }
                    }
                }
            }
            true
        };
        let (r, col) = if let Some(r0) = rs0 {
            let mut c0 = 0;
            ensure(&mut occupied, r0 + rspan, ncols);
            while !fits(&occupied, r0, c0, ncols) && c0 + cspan <= ncols {
                c0 += 1;
            }
            if c0 + cspan > ncols {
                c0 = 0;
            }
            (r0, c0)
        } else if let Some(c0) = cs0 {
            let mut r = if s.grid_auto_flow_dense { 0 } else { cur_r };
            loop {
                ensure(&mut occupied, r + rspan, ncols);
                if fits(&occupied, r, c0, ncols) {
                    break;
                }
                r += 1;
                if r > 10_000 {
                    break;
                }
            }
            cur_r = r;
            (r, c0)
        } else if column_flow {
            let nrows_explicit = rows.len().max(1);
            let (mut r, mut cc) = (cur_r, cur_c);
            loop {
                ensure(&mut occupied, r + rspan, ncols.max(cc + cspan));
                if cc + cspan > ncols {
                    ncols = cc + cspan;
                    ensure(&mut occupied, r + rspan, ncols);
                }
                if fits(&occupied, r, cc, ncols) {
                    break;
                }
                r += 1;
                if r + rspan > nrows_explicit {
                    r = 0;
                    cc += 1;
                }
                if cc > 10_000 {
                    break;
                }
            }
            cur_r = r;
            cur_c = cc;
            (r, cc)
        } else {
            let (mut r, mut cc) = if s.grid_auto_flow_dense { (0, 0) } else { (cur_r, cur_c) };
            loop {
                ensure(&mut occupied, r + rspan, ncols);
                if cc + cspan > ncols {
                    r += 1;
                    cc = 0;
                    continue;
                }
                if fits(&occupied, r, cc, ncols) {
                    break;
                }
                cc += 1;
                if r > 100_000 {
                    break;
                }
            }
            cur_r = r;
            cur_c = cc + cspan;
            (r, cc)
        };
        ensure(&mut occupied, r + rspan, ncols);
        for rr in r..r + rspan {
            for x in col..(col + cspan).min(ncols) {
                occupied[rr][x] = true;
            }
        }
        placed.push(GItem { id: c, col, col_span: cspan, row: r, row_span: rspan });
    }
    let nrows = occupied.len().max(rows.len()).max(1);
    while cols.len() < ncols {
        let i = cols.len() - explicit_cols.min(cols.len());
        cols.push(auto_col[i % auto_col.len()].clone());
    }
    while rows.len() < nrows {
        rows.push(auto_row[rows.len() % auto_row.len()].clone());
    }
    // auto-fit: collapse empty trailing repeated columns.
    let mut collapsed = vec![false; cols.len()];
    if auto_fit {
        for (ci, col) in collapsed.iter_mut().enumerate() {
            if !placed.iter().any(|it| ci >= it.col && ci < it.col + it.col_span) {
                *col = true;
            }
        }
    }

    // Item intrinsic contributions.
    for it in &placed {
        l.resolve_box_metrics(it.id, content_w);
    }
    // (min-content, max-content, minimum contribution). The minimum contribution
    // is what an `auto`-minimum track must fit: the item's specified min-width,
    // or 0 for scroll containers and for items spanning several tracks one of
    // which is flexible, else its content-based minimum (css-grid-1 §6.6).
    let contrib: Vec<(f32, f32, f32)> = placed
        .iter()
        .map(|it| {
            // An item stretched across fixed rows has a definite height while
            // the columns are sized (css-grid-1 §11.5: percentages in it resolve).
            let end_r = (it.row + it.row_span).min(rows.len());
            let fixed_rows: Option<f32> = if it.row < end_r { rows[it.row..end_r].iter().map(|t| if let T::Fixed(v) = t { Some(*v) } else { None }).sum::<Option<f32>>().map(|h| h + row_gap * (end_r - it.row - 1) as f32) } else { None };
            // A single auto/flexible row in a grid of definite height fills it.
            let fixed_rows = fixed_rows.or_else(|| match (rows.len(), content_h_def) {
                (1, Some(ch)) if matches!(&rows[0], T::Auto | T::Fr(_)) || matches!(&rows[0], T::MinMax(_, b) if matches!(**b, T::Auto | T::Fr(_))) => Some(ch),
                _ => None,
            });
            let st0 = l.boxes[it.id].style.clone();
            let stretched = matches!(st0.align_self, Align::Auto | Align::Normal | Align::Stretch) && matches!(if matches!(st0.align_self, Align::Auto) { s.align_items } else { st0.align_self }, Align::Normal | Align::Stretch | Align::Auto);
            let override_h = match fixed_rows {
                Some(area) if st0.height.is_auto() && stretched && !st0.margin[0].is_auto() && !st0.margin[2].is_auto() && !matches!(l.boxes[it.id].kind, BoxKind::Replaced(_)) => {
                    let m = l.boxes[it.id].margin;
                    Some((area - m[0] - m[2]).max(0.0))
                }
                _ => None,
            };
            if let Some(h) = override_h {
                let mut st = (*st0).clone();
                let bb = st.box_sizing == BoxSizing::BorderBox;
                st.height = Len::Px(if bb { h } else { (h - l.vpad(it.id)).max(0.0) });
                l.boxes[it.id].style = Rc::new(st);
                l.clear_intrinsic(it.id);
            }
            let (mn, mx) = l.intrinsic_widths(it.id);
            if override_h.is_some() {
                l.boxes[it.id].style = st0.clone();
                l.clear_intrinsic(it.id);
                l.boxes[it.id].intrinsic.set(Some((mn, mx)));
            }
            let m = l.boxes[it.id].margin[1] + l.boxes[it.id].margin[3];
            let st = &l.boxes[it.id].style;
            let end = (it.col + it.col_span).min(cols.len());
            let spans_flex = it.col_span > 1 && cols[it.col.min(end)..end].iter().any(flexible);
            let minc = if !st.min_width.is_auto() {
                st.min_width.definite(Some(content_w)).map(|v| l.to_border_box_w(it.id, v)).unwrap_or(0.0).min(mn) + m
            } else if st.overflow_x != Overflow::Visible || spans_flex {
                l.hpad(it.id) + m
            } else {
                mn + m
            };
            (mn + m, mx + m, minc)
        })
        .collect();

    // Column sizing; items spanning several columns are fitted inside the
    // algorithm, before free space is distributed.
    let spans: Vec<Span> = placed
        .iter()
        .enumerate()
        .filter(|(_, it)| it.col_span > 1)
        .map(|(k, it)| {
            let end = (it.col + it.col_span).min(cols.len());
            let am = cols[it.col.min(end)..end].iter().any(auto_min);
            Span { start: it.col, end, min: if am { contrib[k].2 } else { contrib[k].0 }, max: contrib[k].1 }
        })
        .collect();
    let col_sizes = size_tracks(&cols, &collapsed, content_w, col_gap, |ci| {
        let mut mn: f32 = 0.0;
        let mut mx: f32 = 0.0;
        let am = auto_min(&cols[ci]);
        for (k, it) in placed.iter().enumerate() {
            if it.col == ci && it.col_span == 1 {
                mn = mn.max(if am { contrib[k].2 } else { contrib[k].0 });
                mx = mx.max(contrib[k].1);
            }
        }
        (mn, mx)
    }, &spans, s.justify_content);
    let col_pos: Vec<f32> = {
        let mut v = Vec::with_capacity(col_sizes.len());
        let mut x = 0.0;
        for (i, c) in col_sizes.iter().enumerate() {
            v.push(x);
            x += c + if collapsed[i] { 0.0 } else { col_gap };
        }
        v
    };
    // Lay out items at their column widths to get heights.
    let mut heights: Vec<f32> = Vec::with_capacity(placed.len());
    let mut min_heights: Vec<f32> = Vec::with_capacity(placed.len());
    let mut item_w: Vec<f32> = Vec::with_capacity(placed.len());
    for it in &placed {
        let end = (it.col + it.col_span).min(col_sizes.len());
        let area_w: f32 = col_sizes[it.col..end].iter().sum::<f32>() + col_gap * (end - it.col).saturating_sub(1) as f32;
        let cs = l.boxes[it.id].style.clone();
        let m = l.boxes[it.id].margin;
        let js = match cs.justify_self {
            Align::Auto => s.justify_items,
            a => a,
        };
        let w = match l.specified_w(it.id, Some(area_w)) {
            Some(w) => w,
            None => {
                if matches!(js, Align::Normal | Align::Stretch) && !cs.margin[1].is_auto() && !cs.margin[3].is_auto() {
                    (area_w - m[1] - m[3]).max(0.0)
                } else {
                    l.shrink_to_fit(it.id, (area_w - m[1] - m[3]).max(0.0))
                }
            }
        };
        let w = l.clamp_w(it.id, w, area_w);
        // Percentage padding resolves against the grid area, not whatever
        // containing block the item was last measured in.
        l.resolve_box_metrics(it.id, area_w);
        let h_spec = l.specified_h(it.id, None);
        l.layout_sized(it.id, Some(w), h_spec, area_w, None);
        heights.push(l.boxes[it.id].outer_h());
        // The minimum contribution of a scroll container is its min-height
        // (its automatic minimum is 0): `auto` / fit-content() rows start there.
        let b = &l.boxes[it.id];
        let mh = if cs.overflow_y != Overflow::Visible {
            let vm = b.margin[0] + b.margin[2];
            let min_h = cs.min_height.definite(None).map(|v| l.to_border_box_h(it.id, v)).unwrap_or(0.0).max(l.vpad(it.id));
            (min_h + vm).min(l.boxes[it.id].outer_h())
        } else {
            l.boxes[it.id].outer_h()
        };
        min_heights.push(mh);
        item_w.push(w);
    }
    if s.subgrid_rows {
        l.boxes[id].sub_rows = Some(Rc::new(placed.iter().enumerate().map(|(k, it)| (it.row, it.row_span, heights[k])).collect()));
    }
    // Row subgrid items contribute their own items to our rows (with their
    // padding and border added to the first and last row they span).
    let mut sub_extra: Vec<(usize, f32)> = Vec::new();
    let is_sub: Vec<bool> = placed.iter().map(|it| l.boxes[it.id].style.subgrid_rows && l.boxes[it.id].sub_rows.is_some()).collect();
    for (k, it) in placed.iter().enumerate() {
        if !is_sub[k] {
            continue;
        }
        let b = &l.boxes[it.id];
        let top = b.border[0] + b.padding[0] + b.margin[0];
        let bottom = b.border[2] + b.padding[2] + b.margin[2];
        // Half the difference between the gutters is extra margin on each side of an inner gutter.
        let half = (b.style.row_gap.definite(None).unwrap_or(0.0).max(0.0) - row_gap) / 2.0;
        for &(r, span, h) in b.sub_rows.as_ref().unwrap().iter() {
            if r >= it.row_span {
                continue;
            }
            let mut h = h;
            if r == 0 {
                h += top;
            } else {
                h += half;
            }
            if r + span >= it.row_span {
                h += bottom;
            } else {
                h += half;
            }
            if span == 1 {
                sub_extra.push((it.row + r, h));
            }
        }
    }
    // Row sizing.
    let no_collapse = vec![false; rows.len()];
    let row_spans: Vec<Span> = placed
        .iter()
        .enumerate()
        // Spanning items crossing a flexible row size the flex fraction; the
        // others still grow the last row they span (below).
        .filter(|(k, it)| it.row_span > 1 && !is_sub[*k] && rows[it.row.min(rows.len())..(it.row + it.row_span).min(rows.len())].iter().any(flexible))
        .map(|(k, it)| Span { start: it.row, end: it.row + it.row_span, min: min_heights[k], max: heights[k] })
        .collect();
    let mut row_sizes = size_tracks(&rows, &no_collapse, content_h_def.unwrap_or(-1.0), row_gap, |ri| {
        let mut mx: f32 = 0.0;
        let mut mn: f32 = 0.0;
        // Only an `auto` minimum uses the minimum contribution; a
        // `min-content` one uses the min-content contribution (the height).
        let am = auto_min(&rows[ri]);
        for (k, it) in placed.iter().enumerate() {
            if it.row == ri && it.row_span == 1 && !is_sub[k] {
                mx = mx.max(heights[k]);
                mn = mn.max(if am { min_heights[k] } else { heights[k] });
            }
        }
        for &(r, h) in &sub_extra {
            if r == ri {
                mx = mx.max(h);
                mn = mn.max(h);
            }
        }
        (mn, mx)
    }, &row_spans, if content_h_def.is_some() { s.align_content } else { Align::Start });
    // A subgrid takes the rows its parent sized.
    if let Some(forced) = l.boxes[id].forced_rows.clone() {
        if forced.len() == row_sizes.len() {
            row_sizes = forced.to_vec();
        }
    }
    for (k, it) in placed.iter().enumerate() {
        if it.row_span > 1 && !is_sub[k] {
            let end = (it.row + it.row_span).min(row_sizes.len());
            let have: f32 = row_sizes[it.row..end].iter().sum::<f32>() + row_gap * (it.row_span - 1) as f32;
            if heights[k] > have {
                let last = end - 1;
                row_sizes[last] += heights[k] - have;
            }
        }
    }
    // Distribution of leftover space between tracks (justify-content /
    // align-content: space-between / around / evenly): (offset, extra per gap).
    let distribute = |a: Align, free: f32, n: usize| -> (f32, f32) {
        if free <= 0.0 || n == 0 {
            return (0.0, 0.0);
        }
        match a {
            Align::Center => (free / 2.0, 0.0),
            Align::End | Align::Right => (free, 0.0),
            Align::SpaceBetween if n > 1 => (0.0, free / (n - 1) as f32),
            Align::SpaceAround => (free / n as f32 / 2.0, free / n as f32),
            Align::SpaceEvenly => (free / (n + 1) as f32, free / (n + 1) as f32),
            _ => (0.0, 0.0),
        }
    };
    let total_h0: f32 = row_sizes.iter().sum::<f32>() + row_gap * (row_sizes.len().saturating_sub(1)) as f32;
    let (y_off, row_extra) = match content_h_def {
        Some(ch) if matches!(s.align_content, Align::Center | Align::End | Align::SpaceBetween | Align::SpaceAround | Align::SpaceEvenly) => distribute(s.align_content, ch - total_h0, row_sizes.len()),
        _ => (0.0, 0.0),
    };
    let row_gap = row_gap + row_extra;
    let row_pos: Vec<f32> = {
        let mut v = Vec::with_capacity(row_sizes.len());
        let mut y = y_off;
        for r in &row_sizes {
            v.push(y);
            y += r + row_gap;
        }
        v
    };
    let total_h: f32 = row_sizes.iter().sum::<f32>() + row_gap * (row_sizes.len().saturating_sub(1)) as f32 + 2.0 * y_off;
    let total_w: f32 = col_sizes.iter().enumerate().map(|(i, c)| c + if collapsed[i] { 0.0 } else { col_gap }).sum::<f32>() - col_gap;
    // justify-content for the grid as a whole (when tracks don't fill).
    let free_w = (content_w - total_w).max(0.0);
    let (x_off, col_extra) = distribute(s.justify_content, free_w, col_sizes.iter().enumerate().filter(|(i, _)| !collapsed[*i]).count());
    let col_gap = col_gap + col_extra;
    let col_pos: Vec<f32> = col_pos.iter().enumerate().map(|(i, p)| p + col_extra * collapsed[..i].iter().filter(|c| !**c).count() as f32).collect();

    // Subgrids lay out again on our rows.
    for (k, it) in placed.iter().enumerate() {
        if !is_sub[k] {
            continue;
        }
        let end_r = (it.row + it.row_span).min(row_sizes.len());
        let (top, bottom, sub_gap) = {
            let b = &l.boxes[it.id];
            let sg = b.style.row_gap.definite(None).unwrap_or(0.0).max(0.0);
            (b.border[0] + b.padding[0] + b.margin[0], b.border[2] + b.padding[2] + b.margin[2], sg)
        };
        let n = end_r - it.row;
        let mut f: Vec<f32> = Vec::with_capacity(n);
        let d = (row_gap - sub_gap) / 2.0;
        for (i, &r) in row_sizes[it.row..end_r].iter().enumerate() {
            let mut v = r;
            if i == 0 {
                v -= top;
            } else {
                v += d;
            }
            if i + 1 == n {
                v -= bottom;
            } else {
                v += d;
            }
            f.push(v.max(0.0));
        }
        let area_h: f32 = row_sizes[it.row..end_r].iter().sum::<f32>() + row_gap * n.saturating_sub(1) as f32;
        let end_c = (it.col + it.col_span).min(col_sizes.len());
        let area_w: f32 = col_sizes[it.col..end_c].iter().sum::<f32>() + col_gap * (end_c - it.col).saturating_sub(1) as f32;
        let m = l.boxes[it.id].margin;
        l.boxes[it.id].forced_rows = Some(Rc::new(f));
        let target = (area_h - m[0] - m[2]).max(0.0);
        l.layout_sized(it.id, Some(item_w[k]), Some(target), area_w, Some(area_h));
        l.boxes[it.id].forced_rows = None;
    }
    // Position items.
    for (k, it) in placed.iter().enumerate() {
        let end_c = (it.col + it.col_span).min(col_sizes.len());
        let end_r = (it.row + it.row_span).min(row_sizes.len());
        let area_w: f32 = col_sizes[it.col..end_c].iter().sum::<f32>() + col_gap * (end_c - it.col).saturating_sub(1) as f32;
        let area_h: f32 = row_sizes[it.row..end_r].iter().sum::<f32>() + row_gap * (end_r - it.row).saturating_sub(1) as f32;
        let cs = l.boxes[it.id].style.clone();
        let m = l.boxes[it.id].margin;
        let als = match cs.align_self {
            Align::Auto => s.align_items,
            a => a,
        };
        let js = match cs.justify_self {
            Align::Auto => s.justify_items,
            a => a,
        };
        // Percentage heights resolve against the grid area (definite once
        // rows are sized), e.g. `height: 100%` filling a stretched row.
        if cs.height.has_pct() && matches!(l.boxes[it.id].kind, BoxKind::Replaced(_)) {
            // Replaced items size against the area directly.
            l.layout_box(it.id, area_w, Some(area_h), None, (0.0, 0.0));
        } else if cs.height.has_pct() {
            if let Some(h) = cs.height.definite(Some(area_h)) {
                // Always: even at the same height, the content was last laid
                // out against an indefinite height (when measuring the row),
                // so its own percentages are unresolved.
                let target = l.clamp_h(it.id, l.to_border_box_h(it.id, h.max(0.0)), Some(area_h));
                l.layout_sized(it.id, Some(item_w[k]), Some(target), area_w, Some(area_h));
            }
        }
        // Stretch height.
        if matches!(als, Align::Normal | Align::Stretch) && cs.height.is_auto() && !cs.margin[0].is_auto() && !cs.margin[2].is_auto() && !matches!(l.boxes[it.id].kind, BoxKind::Replaced(_)) {
            let target = (area_h - m[0] - m[2]).max(0.0);
            // A stretched item's height is definite: children with percentage
            // heights need a layout against it even when it didn't change.
            let pct_kids = l.boxes[it.id].children.iter().any(|&c| l.boxes[c].style.height.has_pct());
            if (l.boxes[it.id].h - target).abs() > 0.5 || pct_kids {
                let target = l.clamp_h(it.id, target, Some(area_h));
                l.layout_sized(it.id, Some(item_w[k]), Some(target), area_w, Some(area_h));
            }
        }
        l.boxes[it.id].cb_size = Some((area_w, area_h));
        let b = &l.boxes[it.id];
        let (bw, bh) = (b.w, b.h);
        let fx = area_w - bw - m[1] - m[3];
        let fy = area_h - bh - m[0] - m[2];
        // Auto margins absorb the free space and override self-alignment
        // (css-grid §11.2): one auto margin pushes the item to the other
        // side, two center it. Offsets are physical (from the left/top).
        let auto_off = |a: bool, b: bool, free: f32| match (a, b) {
            (true, true) => Some(free / 2.0),
            (true, false) => Some(free),
            (false, true) => Some(0.0),
            _ => None,
        };
        let auto_dx = auto_off(cs.margin[3].is_auto(), cs.margin[1].is_auto(), fx);
        let auto_dy = auto_off(cs.margin[0].is_auto(), cs.margin[2].is_auto(), fy);
        let dx = match js {
            Align::Center => fx / 2.0,
            Align::End | Align::Right => fx,
            _ => 0.0,
        };
        let dy = auto_dy.unwrap_or(match als {
            Align::Center => fy / 2.0,
            Align::End => fy,
            _ => 0.0,
        });
        let rtl = s.direction_rtl;
        let b = &mut l.boxes[it.id];
        b.x = if rtl {
            // Columns run right to left; start/end alignment mirrors with them.
            let start = content_w - (x_off + col_pos[it.col] + area_w);
            let dxr = auto_dx.unwrap_or(fx - dx);
            cx + start + m[3] + dxr.max(0.0)
        } else {
            cx + x_off + col_pos[it.col] + m[3] + auto_dx.unwrap_or(dx).max(0.0)
        };
        b.y = cy + row_pos[it.row] + m[0] + dy.max(0.0);
    }
    let mut h = match spec_h {
        Some(h) => h,
        None => total_h + l.vpad(id),
    };
    h = l.clamp_h(id, h, cb_h);
    l.boxes[id].h = h;
    l.finish_block_extras(id);
    // The grid's baseline: that of the first item in the first row (in
    // column order), synthesized from its border-box bottom if it has none.
    if let Some(first) = placed.iter().min_by_key(|it| (it.row, it.col)) {
        let b = &l.boxes[first.id];
        let bl = l.first_baseline(first.id).unwrap_or(b.h) + b.y;
        l.boxes[id].baseline = Some(bl);
    }
}

/// Track sizing for one axis. `avail` < 0 means indefinite.
/// An item spanning tracks `start..end` with minimum and max-content contributions.
struct Span {
    start: usize,
    end: usize,
    min: f32,
    max: f32,
}

fn size_tracks(tracks: &[T], collapsed: &[bool], avail: f32, gap: f32, contrib: impl Fn(usize) -> (f32, f32), spans: &[Span], justify: Align) -> Vec<f32> {
    let n = tracks.len();
    let mut base = vec![0.0f32; n];
    let mut limit = vec![f32::INFINITY; n];
    let mut fr = vec![0.0f32; n];
    for (i, t) in tracks.iter().enumerate() {
        if collapsed.get(i).copied().unwrap_or(false) {
            limit[i] = 0.0;
            continue;
        }
        let (cmin, cmax) = contrib(i);
        match t {
            T::Fixed(v) => {
                base[i] = *v;
                limit[i] = *v;
            }
            T::Fr(f) => {
                fr[i] = *f;
                // `1fr` is minmax(auto, 1fr): its minimum is the items' minimum contribution.
                base[i] = cmin;
            }
            T::Auto => {
                base[i] = cmin;
                limit[i] = cmax.max(cmin);
            }
            T::MinContent => {
                base[i] = cmin;
                limit[i] = cmin;
            }
            T::MaxContent => {
                base[i] = cmax;
                limit[i] = cmax;
            }
            T::Fit(v) => {
                base[i] = cmin;
                limit[i] = cmax.min(*v).max(cmin);
            }
            T::MinMax(a, b) => {
                base[i] = match **a {
                    T::Fixed(v) => v,
                    T::MinContent | T::Auto => cmin,
                    T::MaxContent => cmax,
                    _ => 0.0,
                };
                match **b {
                    T::Fr(f) => fr[i] = f,
                    T::Fixed(v) => limit[i] = v.max(base[i]),
                    T::MinContent => limit[i] = cmin.max(base[i]),
                    T::MaxContent | T::Auto => limit[i] = cmax.max(base[i]),
                    _ => {}
                }
                if fr[i] > 0.0 {
                    limit[i] = f32::INFINITY;
                }
            }
        }
    }
    // Items spanning several tracks (css-grid-1 §12.5 steps 3-4), smallest spans
    // first: those crossing a flexible track grow only the flexible tracks (in
    // proportion to their flex factors); others grow the tracks with an intrinsic
    // minimum, then the growth limits of tracks with an intrinsic maximum.
    let mut order: Vec<&Span> = spans.iter().collect();
    order.sort_by_key(|sp| sp.end - sp.start);
    for sp in order {
        let range = sp.start..sp.end.min(n);
        let inner_gaps = gap * range.len().saturating_sub(1) as f32;
        let have: f32 = base[range.clone()].iter().sum::<f32>() + inner_gaps;
        let flex: Vec<usize> = range.clone().filter(|&i| fr[i] > 0.0 && intrinsic_min(&tracks[i])).collect();
        if range.clone().any(|i| fr[i] > 0.0) {
            if !flex.is_empty() && sp.min > have + 0.5 {
                let total: f32 = flex.iter().map(|&i| fr[i]).sum();
                let extra = sp.min - have;
                for &i in &flex {
                    base[i] += if total >= 1.0 { extra * fr[i] / total } else { extra / flex.len() as f32 };
                }
            }
            continue;
        }
        let grow: Vec<usize> = range.clone().filter(|&i| intrinsic_min(&tracks[i])).collect();
        if !grow.is_empty() && sp.min > have + 0.5 {
            // Distribute equally, freezing tracks at their growth limits
            // (§12.5.1); what's left goes past the limits of the tracks with
            // an intrinsic maximum (or of all of them).
            let mut extra = sp.min - have;
            let mut open: Vec<usize> = grow.iter().copied().filter(|&i| limit[i] > base[i] + 0.01).collect();
            for _ in 0..=grow.len() {
                if open.is_empty() || extra <= 0.01 {
                    break;
                }
                let share = extra / open.len() as f32;
                let mut next = Vec::new();
                for &i in &open {
                    let room = if limit[i].is_finite() { limit[i] - base[i] } else { f32::INFINITY };
                    let add = share.min(room);
                    base[i] += add;
                    extra -= add;
                    if room - add > 0.01 {
                        next.push(i);
                    }
                }
                open = next;
            }
            if extra > 0.01 {
                let beyond: Vec<usize> = {
                    let intrinsic_max: Vec<usize> = grow
                        .iter()
                        .copied()
                        .filter(|&i| match &tracks[i] {
                            T::Auto | T::MinContent | T::MaxContent | T::Fit(_) => true,
                            T::MinMax(_, b) => matches!(**b, T::Auto | T::MinContent | T::MaxContent),
                            _ => false,
                        })
                        .collect();
                    if intrinsic_max.is_empty() { grow.clone() } else { intrinsic_max }
                };
                let add = extra / beyond.len() as f32;
                for &i in &beyond {
                    base[i] += add;
                }
            }
            for &i in &grow {
                if limit[i] < base[i] {
                    limit[i] = base[i];
                }
            }
        }
        let lim_have: f32 = range.clone().map(|i| if limit[i].is_finite() { limit[i] } else { base[i] }).sum::<f32>() + inner_gaps;
        let grow_max: Vec<usize> = range
            .clone()
            .filter(|&i| match &tracks[i] {
                T::Auto | T::MaxContent | T::Fit(_) => true,
                T::MinMax(_, b) => matches!(**b, T::Auto | T::MaxContent),
                _ => false,
            })
            .collect();
        if !grow_max.is_empty() && sp.max > lim_have + 0.5 {
            let add = (sp.max - lim_have) / grow_max.len() as f32;
            for &i in &grow_max {
                let cur = if limit[i].is_finite() { limit[i] } else { base[i] };
                limit[i] = match &tracks[i] {
                    T::Fit(v) => (cur + add).min(v.max(base[i])),
                    _ => cur + add,
                };
            }
        }
    }
    let active = |i: usize| !collapsed.get(i).copied().unwrap_or(false);
    let gaps = gap * ((0..n).filter(|&i| active(i)).count().saturating_sub(1)) as f32;
    let mut sizes = base.clone();
    if avail >= 0.0 {
        // Grow non-flexible tracks toward their limits.
        let mut free = avail - sizes.iter().sum::<f32>() - gaps;
        if free > 0.0 {
            // "Maximize tracks": share the free space equally, freezing tracks
            // as they reach their growth limits (CSS Grid §12.6).
            let mut growable: Vec<usize> = (0..n).filter(|&i| fr[i] == 0.0 && limit[i].is_finite() && limit[i] > sizes[i] && active(i)).collect();
            for _ in 0..=n {
                if growable.is_empty() || free <= 0.01 {
                    break;
                }
                let share = free / growable.len() as f32;
                let mut next = Vec::new();
                for &i in &growable {
                    let add = share.min(limit[i] - sizes[i]);
                    sizes[i] += add;
                    free -= add;
                    if limit[i] - sizes[i] > 0.01 {
                        next.push(i);
                    }
                }
                growable = next;
            }
        }
        // Flexible tracks.
        let total_fr: f32 = fr.iter().sum();
        if total_fr > 0.0 {
            let mut excluded = vec![false; n];
            for _ in 0..n.max(1) {
                let fixed_sum: f32 = (0..n).filter(|&i| fr[i] == 0.0 || excluded[i]).map(|i| sizes[i]).sum();
                let frs: f32 = (0..n).filter(|&i| fr[i] > 0.0 && !excluded[i]).map(|i| fr[i]).sum();
                let left = (avail - fixed_sum - gaps).max(0.0);
                let unit = if frs > 0.0 { left / frs.max(1.0) } else { 0.0 };
                let mut changed = false;
                for i in 0..n {
                    if fr[i] > 0.0 && !excluded[i] {
                        let v = unit * fr[i];
                        if v < base[i] {
                            excluded[i] = true;
                            sizes[i] = base[i];
                            changed = true;
                        } else {
                            sizes[i] = v;
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
        } else if matches!(justify, Align::Normal | Align::Stretch) {
            // Stretch auto tracks to fill remaining space.
            let free = avail - sizes.iter().sum::<f32>() - gaps;
            let autos: Vec<usize> = (0..n).filter(|&i| matches!(tracks[i], T::Auto) && active(i)).collect();
            if free > 0.0 && !autos.is_empty() {
                let add = free / autos.len() as f32;
                for i in autos {
                    sizes[i] += add;
                }
            }
        }
    } else {
        // Indefinite: non-flexible tracks grow to their limits; flexible
        // tracks share one flex fraction, the largest any item needs
        // (css-grid-1 §12.7.1, "find the size of an fr" per item).
        for i in 0..n {
            if fr[i] == 0.0 {
                if matches!(tracks[i], T::Auto) {
                    let (_, cmax) = contrib(i);
                    sizes[i] = sizes[i].max(cmax);
                }
                if limit[i].is_finite() && limit[i] > sizes[i] {
                    sizes[i] = limit[i];
                }
            }
        }
        if fr.iter().any(|&f| f > 0.0) {
            let mut unit: f32 = 0.0;
            for i in 0..n {
                if fr[i] > 0.0 && active(i) {
                    let (_, cmax) = contrib(i);
                    let c = cmax.max(base[i]);
                    unit = unit.max(if fr[i] >= 1.0 { c / fr[i] } else { c });
                }
            }
            for sp in spans {
                let range = sp.start..sp.end.min(n);
                let frs: f32 = range.clone().filter(|&i| fr[i] > 0.0).map(|i| fr[i]).sum();
                if frs <= 0.0 {
                    continue;
                }
                let fixed: f32 = range.clone().filter(|&i| fr[i] == 0.0).map(|i| sizes[i]).sum::<f32>() + gap * range.len().saturating_sub(1) as f32;
                unit = unit.max((sp.max - fixed).max(0.0) / frs.max(1.0));
            }
            for i in 0..n {
                if fr[i] > 0.0 {
                    sizes[i] = sizes[i].max(unit * fr[i]);
                }
            }
        }
    }
    for (i, s) in sizes.iter_mut().enumerate() {
        if !active(i) {
            *s = 0.0;
        }
    }
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fr_min0() -> T {
        T::MinMax(Box::new(T::Fixed(0.0)), Box::new(T::Fr(1.0)))
    }

    #[test]
    fn spanning_item_does_not_grow_fixed_minimum_tracks() {
        // repeat(4, minmax(0, 1fr)) with a 900px item spanning three columns.
        let tracks = vec![fr_min0(), fr_min0(), fr_min0(), fr_min0()];
        let spans = [Span { start: 0, end: 3, min: 900.0, max: 900.0 }];
        let sizes = size_tracks(&tracks, &[false; 4], 600.0, 10.0, |_| (0.0, 0.0), &spans, Align::Normal);
        assert!(sizes.iter().all(|&s| (s - 142.5).abs() < 0.01), "{sizes:?}");
    }

    #[test]
    fn spanning_item_grows_auto_tracks_before_fr() {
        // 100px auto 1fr, item of 400px spanning the first two.
        let tracks = vec![T::Fixed(100.0), T::Auto, T::Fr(1.0)];
        let spans = [Span { start: 0, end: 2, min: 400.0, max: 400.0 }];
        let sizes = size_tracks(&tracks, &[false; 3], 600.0, 10.0, |_| (0.0, 0.0), &spans, Align::Normal);
        assert_eq!(sizes, vec![100.0, 290.0, 190.0]);
    }
}
