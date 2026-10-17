// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Flexible Box Layout.

use super::*;

impl<'a> Layout<'a> {
    /// Lay out `id` with forced border-box width and/or height.
    pub fn layout_sized(&mut self, id: BoxId, w: Option<f32>, h: Option<f32>, cb_w: f32, cb_h: Option<f32>) {
        lstat(1);
        let orig = self.boxes[id].style.clone();
        if w.is_some() || h.is_some() {
            let mut st = (*orig).clone();
            let bb = orig.box_sizing == BoxSizing::BorderBox;
            self.resolve_box_metrics(id, cb_w);
            if let Some(w) = w {
                st.width = Len::Px(if bb { w } else { (w - self.hpad(id)).max(0.0) });
                st.min_width = Len::Auto;
                st.max_width = Len::None;
            }
            if let Some(h) = h {
                st.height = Len::Px(if bb { h } else { (h - self.vpad(id)).max(0.0) });
                st.min_height = Len::Auto;
                st.max_height = Len::None;
            }
            self.boxes[id].style = Rc::new(st);
        }
        // The intrinsic sizes under the forced size are not the box's own:
        // keep the real ones (if known) for after.
        let saved_intrinsic = self.boxes[id].intrinsic.get();
        let forced = w.is_some() || h.is_some();
        if forced {
            self.boxes[id].intrinsic.set(None);
        }
        self.layout_box(id, cb_w, cb_h, None, (0.0, 0.0));
        if forced {
            self.boxes[id].intrinsic.set(saved_intrinsic);
        }
        // Restore, keeping the margins computed against the real style.
        let mg = self.boxes[id].margin;
        self.boxes[id].style = orig;
        self.boxes[id].margin = mg;
        if let Some(w) = w {
            self.boxes[id].w = w;
        }
        if let Some(h) = h {
            self.boxes[id].h = h;
        }
    }
}


struct FlexItem {
    id: BoxId,
    base: f32,
    /// Flex base size of the content box (scales flex-shrink).
    inner_base: f32,
    hyp: f32,
    min: f32,
    max: f32,
    target: f32,
    frozen: bool,
    grow: f32,
    shrink: f32,
    margin_main: (f32, f32),
    auto_main: (bool, bool),
    margin_cross: (f32, f32),
    auto_cross: (bool, bool),
    cross: f32,
    baseline: f32,
    align: Align,
    /// The flex base size came from the item's content.
    content_basis: bool,
}

impl FlexItem {
    fn outer_hyp(&self) -> f32 {
        self.hyp + self.margin_main.0 + self.margin_main.1
    }
    fn outer_target(&self) -> f32 {
        self.target + self.margin_main.0 + self.margin_main.1
    }
}

/// Whether a percentage height somewhere inside `id` would resolve against
/// its height once that height is definite (a stretched flex item's is).
fn pct_height_inside(l: &Layout, id: BoxId, depth: u32) -> bool {
    let container = matches!(l.boxes[id].style.display, Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid);
    for &ch in &l.boxes[id].children {
        let st = &l.boxes[ch].style;
        if st.height.has_pct() || st.min_height.has_pct() || st.max_height.has_pct() {
            return true;
        }
        let anon = l.boxes[ch].node.is_none();
        if depth < 4 && st.height.is_auto() && (container || anon) && pct_height_inside(l, ch, depth + 1) {
            return true;
        }
    }
    false
}

pub fn layout_flex(l: &mut Layout, id: BoxId, cb_w: f32, cb_h: Option<f32>) {
    l.resolve_box_metrics(id, cb_w);
    let w = l.compute_block_width(id, cb_w);
    l.boxes[id].w = w;
    let s = l.boxes[id].style.clone();
    let content_w = (w - l.hpad(id)).max(0.0);
    let spec_h = l.specified_h(id, cb_h);
    let content_h_def = spec_h.map(|h| (l.clamp_h(id, h, cb_h) - l.vpad(id)).max(0.0));
    let row = s.flex_direction.is_row();
    // In a right-to-left container the row's main-start is on the right.
    let reverse = s.flex_direction.is_reverse() ^ (s.flex_direction.is_row() && s.direction_rtl);
    let wrap = s.flex_wrap != FlexWrap::NoWrap;
    let cx = l.boxes[id].content_x();
    let cy = l.boxes[id].content_y();

    let gap_main = if row { s.column_gap.definite(Some(content_w)) } else { s.row_gap.definite(content_h_def) }.unwrap_or(0.0).max(0.0);
    let gap_cross = if row { s.row_gap.definite(content_h_def) } else { s.column_gap.definite(Some(content_w)) }.unwrap_or(0.0).max(0.0);

    let mut kids: Vec<BoxId> = Vec::new();
    for &c in &l.boxes[id].children.clone() {
        if l.boxes[c].out_of_flow {
            l.register_oof(c, cx, cy);
        } else {
            kids.push(c);
        }
    }
    kids.sort_by_key(|&c| l.boxes[c].style.order);

    let main_size: Option<f32> = if row { Some(content_w) } else { content_h_def };
    let container_max_main = if row { None } else { s.max_height.definite(cb_h).map(|m| (l.to_border_box_h(id, m) - l.vpad(id)).max(0.0)) };
    // An auto-height column container still grows to its min-height, and items flex into that space.
    let container_min_main = if row { None } else { s.min_height.definite(cb_h).map(|m| (l.to_border_box_h(id, m) - l.vpad(id)).max(0.0)) };

    let mut items: Vec<FlexItem> = Vec::with_capacity(kids.len());
    for &c in &kids {
        l.resolve_box_metrics(c, content_w);
        let cs = l.boxes[c].style.clone();
        let m = l.boxes[c].margin;
        let (mm, am, mc, ac) = if row {
            ((m[3], m[1]), (cs.margin[3].is_auto(), cs.margin[1].is_auto()), (m[0], m[2]), (cs.margin[0].is_auto(), cs.margin[2].is_auto()))
        } else {
            ((m[0], m[2]), (cs.margin[0].is_auto(), cs.margin[2].is_auto()), (m[3], m[1]), (cs.margin[3].is_auto(), cs.margin[1].is_auto()))
        };
        let align = match cs.align_self {
            Align::Auto => s.align_items,
            a => a,
        };
        let align = match align {
            Align::Normal => Align::Stretch,
            Align::Left => Align::Start,
            Align::Right => Align::End,
            a => a,
        };
        // Flex base size.
        let basis_def: Option<f32> = match &cs.flex_basis {
            Len::Auto => {
                if row {
                    l.specified_w(c, Some(content_w))
                } else {
                    l.specified_h(c, content_h_def)
                }
            }
            Len::MaxContent | Len::MinContent | Len::FitContent => None,
            b => b.definite(main_size).map(|v| if row { l.to_border_box_w(c, v.max(0.0)) } else { l.to_border_box_h(c, v.max(0.0)) }),
        };
        let is_replaced = matches!(l.boxes[c].kind, BoxKind::Replaced(_));
        let base = match basis_def {
            Some(b) => b,
            None => {
                if row {
                    if is_replaced {
                        l.replaced_size(c, Some(content_w), content_h_def).0
                    } else {
                        let (mn, mx) = l.intrinsic_widths(c);
                        match cs.flex_basis {
                            Len::MinContent => mn,
                            Len::FitContent => mx.min(content_w.max(mn)),
                            _ => mx,
                        }
                    }
                } else {
                    // Lay out at the cross size to measure content height.
                    let cross_w = match l.specified_w(c, Some(content_w)) {
                        Some(w) => w,
                        None => {
                            if align == Align::Stretch && !ac.0 && !ac.1 {
                                (content_w - mc.0 - mc.1).max(0.0)
                            } else {
                                l.shrink_to_fit(c, (content_w - mc.0 - mc.1).max(0.0))
                            }
                        }
                    };
                    // Measure at the width the item will really get, which
                    // respects its min/max-width (layout_sized drops them).
                    let cross_w = l.clamp_w(c, cross_w, content_w);
                    // A content-based flex basis ignores the item's own height
                    // (e.g. `flex: 1 1 0%` in an auto-height column with `height` set).
                    let real = l.boxes[c].style.clone();
                    let sized = !matches!(real.height, Len::Auto) || !matches!(real.max_height, Len::None);
                    if sized {
                        let mut st = (*real).clone();
                        st.height = Len::Auto;
                        st.max_height = Len::None;
                        st.min_height = Len::Auto;
                        l.boxes[c].style = std::rc::Rc::new(st);
                    }
                    l.layout_sized(c, Some(cross_w), None, content_w, content_h_def);
                    if sized {
                        l.boxes[c].style = real;
                    }
                    l.boxes[c].h
                }
            }
        };
        // min/max main.
        let (min_spec, max_spec) = if row {
            (cs.min_width.definite(Some(content_w)).map(|v| l.to_border_box_w(c, v)), cs.max_width.definite(Some(content_w)).map(|v| l.to_border_box_w(c, v)))
        } else {
            (cs.min_height.definite(content_h_def).map(|v| l.to_border_box_h(c, v)), cs.max_height.definite(content_h_def).map(|v| l.to_border_box_h(c, v)))
        };
        let min = match min_spec {
            Some(v) => v,
            None => {
                if cs.min_width.is_auto() && row && !cs.overflow_x.clips() {
                    // Form controls with percentage widths are "compressible": their content-based
                    // minimum is the intrinsic size, not the percentage-resolved one.
                    let compressible = matches!(&l.boxes[c].kind, BoxKind::Replaced(k) if matches!(k, super::ReplacedKind::TextInput | super::ReplacedKind::TextArea | super::ReplacedKind::Select | super::ReplacedKind::Button | super::ReplacedKind::DateTime | super::ReplacedKind::File))
                        && matches!(cs.width, Len::Pct(_) | Len::Calc(..) | Len::Expr(_));
                    let mc_w = if is_replaced {
                        base.min(l.replaced_size(c, if compressible { None } else { Some(content_w) }, None).0)
                    } else {
                        l.content_min_width(c)
                    };
                    let m = match l.specified_w(c, Some(content_w)) {
                        Some(sw) => mc_w.min(sw),
                        None => mc_w,
                    };
                    // The content-based minimum is clamped by a definite max main size.
                    match max_spec {
                        Some(mx) => m.min(mx),
                        None => m,
                    }
                } else if !row && cs.min_height.is_auto() && !cs.overflow_y.clips() && basis_def.is_none() {
                    // Content-based minimum: the smaller of the content size and a specified height.
                    let m = match l.specified_h(c, content_h_def) {
                        Some(sh) => base.min(sh),
                        None => base,
                    };
                    match max_spec {
                        Some(mx) => m.min(mx),
                        None => m,
                    }
                } else if !row && cs.min_height.is_auto() && !cs.overflow_y.clips() && main_size.is_none() {
                    0.0
                } else {
                    if row { l.hpad(c) } else { l.vpad(c) }
                }
            }
        };
        let max = max_spec.unwrap_or(f32::INFINITY);
        let hyp = base.min(max).max(min);
        let pad_main = if row { l.hpad(c) } else { l.vpad(c) };
        items.push(FlexItem {
            id: c,
            base,
            inner_base: (base - pad_main).max(0.0),
            hyp,
            min,
            max,
            target: hyp,
            frozen: false,
            grow: cs.flex_grow,
            shrink: cs.flex_shrink,
            margin_main: mm,
            auto_main: am,
            margin_cross: mc,
            auto_cross: ac,
            cross: 0.0,
            baseline: 0.0,
            align,
            content_basis: basis_def.is_none(),
        });
    }

    // Collect lines.
    let mut lines: Vec<Vec<usize>> = Vec::new();
    {
        let limit = main_size.or(container_max_main);
        let mut cur: Vec<usize> = Vec::new();
        let mut used = 0.0;
        for (i, it) in items.iter().enumerate() {
            let add = it.outer_hyp() + if cur.is_empty() { 0.0 } else { gap_main };
            if wrap && !cur.is_empty() {
                if let Some(lim) = limit {
                    if used + add > lim + 0.5 {
                        lines.push(std::mem::take(&mut cur));
                        used = 0.0;
                    }
                }
            }
            used += it.outer_hyp() + if cur.is_empty() { 0.0 } else { gap_main };
            cur.push(i);
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }

    // Resolve flexible lengths per line.
    let mut used_main_total: f32 = 0.0;
    for line in &lines {
        let gaps = gap_main * (line.len().saturating_sub(1)) as f32;
        let sum_hyp: f32 = line.iter().map(|&i| items[i].outer_hyp()).sum::<f32>() + gaps;
        let avail_main = match main_size {
            Some(m) => m,
            None => {
                let v = match container_min_main {
                    Some(mn) if lines.len() == 1 => sum_hyp.max(mn),
                    _ => sum_hyp,
                };
                match container_max_main {
                    Some(mx) if v > mx => mx,
                    _ => v,
                }
            }
        };
        let grow = avail_main > sum_hyp;
        for &i in line {
            let it = &mut items[i];
            it.frozen = false;
            if (grow && it.grow == 0.0) || (!grow && it.shrink == 0.0) || (grow && it.base > it.hyp) || (!grow && it.base < it.hyp) {
                it.frozen = true;
                it.target = it.hyp;
            }
        }
        for _ in 0..20 {
            let unfrozen: Vec<usize> = line.iter().copied().filter(|&i| !items[i].frozen).collect();
            if unfrozen.is_empty() {
                break;
            }
            let used: f32 = line
                .iter()
                .map(|&i| {
                    let it = &items[i];
                    if it.frozen {
                        it.outer_target()
                    } else {
                        it.base + it.margin_main.0 + it.margin_main.1
                    }
                })
                .sum::<f32>()
                + gaps;
            let mut free = avail_main - used;
            if grow {
                let sum_g: f32 = unfrozen.iter().map(|&i| items[i].grow).sum();
                if sum_g < 1.0 {
                    free = free.min((avail_main - sum_hyp) * sum_g).max(free * sum_g);
                }
                for &i in &unfrozen {
                    let it = &mut items[i];
                    it.target = it.base + if sum_g > 0.0 { free * it.grow / sum_g } else { 0.0 };
                }
            } else {
                let sum_s: f32 = unfrozen.iter().map(|&i| items[i].shrink * items[i].inner_base).sum();
                for &i in &unfrozen {
                    let it = &mut items[i];
                    it.target = it.base + if sum_s > 0.0 { free * (it.shrink * it.inner_base) / sum_s } else { 0.0 };
                }
            }
            let mut total_violation = 0.0;
            let mut viol: Vec<(usize, f32)> = Vec::new();
            for &i in &unfrozen {
                let it = &mut items[i];
                let clamped = it.target.min(it.max).max(it.min);
                let v = clamped - it.target;
                total_violation += v;
                viol.push((i, v));
                it.target = clamped;
            }
            if total_violation.abs() < 0.01 {
                for &i in &unfrozen {
                    items[i].frozen = true;
                }
                break;
            }
            for (i, v) in viol {
                if (total_violation > 0.0 && v > 0.0) || (total_violation < 0.0 && v < 0.0) {
                    items[i].frozen = true;
                }
            }
        }
        let used: f32 = line.iter().map(|&i| items[i].outer_target()).sum::<f32>() + gaps;
        used_main_total = used_main_total.max(used);
    }

    // Cross sizes: lay out items at their main size.
    let container_cross_def: Option<f32> = if row { content_h_def } else { Some(content_w) };
    for line in &lines {
        for &i in line {
            let it = &items[i];
            let c = it.id;
            if row {
                let h_def = l.specified_h(c, content_h_def).map(|h| l.clamp_h(c, h, content_h_def));
                l.layout_sized(c, Some(it.target), h_def, content_w, content_h_def);
            } else {
                let cross_w = match l.specified_w(c, Some(content_w)) {
                    Some(w) => w,
                    None => {
                        if it.align == Align::Stretch && !it.auto_cross.0 && !it.auto_cross.1 {
                            (content_w - it.margin_cross.0 - it.margin_cross.1).max(0.0)
                        } else {
                            l.shrink_to_fit(c, (content_w - it.margin_cross.0 - it.margin_cross.1).max(0.0))
                        }
                    }
                };
                let cross_w = l.clamp_w(c, cross_w, content_w);
                // In an auto-height column an unflexed, content-sized item keeps
                // an indefinite height (css-flexbox §9.8): percentages inside
                // it don't resolve against its laid-out size.
                let natural = main_size.is_none() && it.content_basis && l.specified_h(c, None).is_none() && (it.target - it.base).abs() < 0.5;
                l.layout_sized(c, Some(cross_w), if natural { None } else { Some(it.target) }, content_w, content_h_def);
            }
            let b = &l.boxes[c];
            let cross = if row { b.h } else { b.w };
            let bl = if row { l.first_baseline(c).unwrap_or(b.h) } else { 0.0 };
            let it = &mut items[i];
            it.cross = cross;
            it.baseline = bl + it.margin_cross.0;
        }
    }
    // Line cross sizes.
    let mut line_cross: Vec<f32> = Vec::new();
    let mut line_base: Vec<f32> = Vec::new();
    for line in &lines {
        let mut mx: f32 = 0.0;
        let mut max_a: f32 = 0.0;
        let mut max_d: f32 = 0.0;
        for &i in line {
            let it = &items[i];
            let outer = it.cross + it.margin_cross.0 + it.margin_cross.1;
            if it.align == Align::Baseline && row {
                max_a = max_a.max(it.baseline);
                max_d = max_d.max(outer - it.baseline);
            } else {
                mx = mx.max(outer);
            }
        }
        line_cross.push(mx.max(max_a + max_d));
        line_base.push(max_a);
    }
    // The container's min/max cross sizes (content box) for an auto cross size.
    let (min_cross, max_cross) = if row {
        (
            s.min_height.definite(cb_h).map(|m| (l.to_border_box_h(id, m) - l.vpad(id)).max(0.0)),
            s.max_height.definite(cb_h).map(|m| (l.to_border_box_h(id, m) - l.vpad(id)).max(0.0)),
        )
    } else {
        (None, None)
    };
    if lines.len() == 1 && !wrap {
        if let Some(cc) = container_cross_def {
            line_cross[0] = cc;
        } else {
            // A single-line container's line is clamped by the container's
            // min/max cross sizes (css-flexbox §9.4 step 8).
            if let Some(mx) = max_cross {
                line_cross[0] = line_cross[0].min(mx);
            }
            if let Some(mn) = min_cross {
                line_cross[0] = line_cross[0].max(mn);
            }
        }
    } else if lines.len() == 1 {
        if let Some(cc) = container_cross_def {
            line_cross[0] = cc;
        }
    }
    let total_cross: f32 = line_cross.iter().sum::<f32>() + gap_cross * (lines.len().saturating_sub(1)) as f32;
    // Container size.
    let content_cross = match container_cross_def {
        Some(c) => c,
        None => {
            let mut c = total_cross;
            if let Some(mx) = max_cross {
                c = c.min(mx);
            }
            if let Some(mn) = min_cross {
                c = c.max(mn);
            }
            c
        }
    };
    let content_main = match main_size {
        Some(m) => m,
        None => {
            let m = used_main_total.max(container_min_main.unwrap_or(0.0));
            match container_max_main {
                Some(mx) => m.min(mx),
                None => m,
            }
        }
    };
    // align-content: distribute free cross space among lines.
    let mut line_offsets: Vec<f32> = Vec::new();
    {
        let free = (content_cross - total_cross).max(0.0);
        let n = lines.len() as f32;
        let (mut start, mut between) = (0.0, 0.0);
        let ac = if lines.len() > 1 || wrap { s.align_content } else { Align::Stretch };
        match ac {
            Align::Normal | Align::Stretch => {
                if lines.len() > 1 || wrap {
                    let add = free / n;
                    for c in line_cross.iter_mut() {
                        *c += add;
                    }
                }
            }
            Align::Center => start = free / 2.0,
            Align::End => start = free,
            Align::SpaceBetween => between = if n > 1.0 { free / (n - 1.0) } else { 0.0 },
            Align::SpaceAround => {
                between = free / n;
                start = between / 2.0;
            }
            Align::SpaceEvenly => {
                between = free / (n + 1.0);
                start = between;
            }
            _ => {}
        }
        let mut pos = start;
        for (i, c) in line_cross.iter().enumerate() {
            line_offsets.push(pos);
            pos += c + gap_cross + between;
            let _ = i;
        }
        if s.flex_wrap == FlexWrap::WrapReverse {
            let total = pos - gap_cross - between;
            for (i, o) in line_offsets.iter_mut().enumerate() {
                *o = total - *o - line_cross[i];
            }
        }
    }

    // Stretch + main-axis positioning.
    for (li, line) in lines.iter().enumerate() {
        let lc = line_cross[li];
        // Stretch items.
        for &i in line {
            let it = &items[i];
            let c = it.id;
            let cross_auto = if row { l.boxes[c].style.height.is_auto() } else { l.boxes[c].style.width.is_auto() };
            if it.align == Align::Stretch && cross_auto && !it.auto_cross.0 && !it.auto_cross.1 {
                let target_cross = (lc - it.margin_cross.0 - it.margin_cross.1).max(0.0);
                let target_cross = if row { l.clamp_h(c, target_cross, content_h_def) } else { l.clamp_w(c, target_cross, content_w) };
                // Same size, but a measured (indefinite) height becomes definite:
                // percentages inside now resolve against it.
                if (target_cross - it.cross).abs() > 0.01 || (row && pct_height_inside(l, c, 0)) {
                    if row {
                        l.layout_sized(c, Some(it.target), Some(target_cross), content_w, Some(target_cross));
                    } else {
                        let natural = main_size.is_none() && it.content_basis && l.specified_h(c, None).is_none() && (it.target - it.base).abs() < 0.5;
                        l.layout_sized(c, Some(target_cross), if natural { None } else { Some(it.target) }, content_w, content_h_def);
                    }
                    items[i].cross = target_cross;
                }
            }
        }
        // Main axis.
        let gaps = gap_main * (line.len().saturating_sub(1)) as f32;
        let used: f32 = line.iter().map(|&i| items[i].outer_target()).sum::<f32>() + gaps;
        let mut free = content_main - used;
        let auto_count: usize = line.iter().map(|&i| items[i].auto_main.0 as usize + items[i].auto_main.1 as usize).sum();
        let mut auto_share = 0.0;
        if free > 0.0 && auto_count > 0 {
            auto_share = free / auto_count as f32;
            free = 0.0;
        }
        let n = line.len() as f32;
        let jc = s.justify_content;
        let (mut pos, between) = match jc {
            Align::Center => (free / 2.0, 0.0),
            Align::End | Align::Right => (free, 0.0),
            Align::SpaceBetween => (0.0, if n > 1.0 && free > 0.0 { free / (n - 1.0) } else { 0.0 }),
            Align::SpaceAround => {
                if free > 0.0 {
                    (free / n / 2.0, free / n)
                } else {
                    (free / 2.0, 0.0)
                }
            }
            Align::SpaceEvenly => {
                if free > 0.0 {
                    (free / (n + 1.0), free / (n + 1.0))
                } else {
                    (free / 2.0, 0.0)
                }
            }
            _ => (0.0, 0.0),
        };
        // flex-end with reverse direction: items pack from the end.
        if reverse && matches!(jc, Align::Normal | Align::Start) {
            pos = free;
        }
        let order: Vec<usize> = if reverse { line.iter().rev().copied().collect() } else { line.clone() };
        // For reversed directions, Start/End are swapped: compensate.
        if reverse && matches!(jc, Align::End) {
            pos = 0.0;
        }
        for &i in &order {
            let it = &items[i];
            let ml = it.margin_main.0 + if it.auto_main.0 { auto_share } else { 0.0 };
            let mr = it.margin_main.1 + if it.auto_main.1 { auto_share } else { 0.0 };
            let main_pos = pos + ml;
            pos += ml + it.target + mr + gap_main + between;
            // Cross position.
            let outer_cross = it.cross + it.margin_cross.0 + it.margin_cross.1;
            let cfree = lc - outer_cross;
            let cross_pos = if it.auto_cross.0 && it.auto_cross.1 {
                (cfree / 2.0).max(0.0) + it.margin_cross.0
            } else if it.auto_cross.0 {
                cfree.max(0.0) + it.margin_cross.0
            } else if it.auto_cross.1 && cfree >= 0.0 {
                // An auto end margin absorbs the free space: the item sits at the start.
                it.margin_cross.0
            } else {
                let al = if s.flex_wrap == FlexWrap::WrapReverse {
                    match it.align {
                        Align::Start => Align::End,
                        Align::End => Align::Start,
                        a => a,
                    }
                } else {
                    it.align
                };
                match al {
                    Align::Center => cfree / 2.0 + it.margin_cross.0,
                    Align::End => cfree + it.margin_cross.0,
                    Align::Baseline if row => line_base[li] - it.baseline + it.margin_cross.0,
                    _ => it.margin_cross.0,
                }
            };
            let b = &mut l.boxes[it.id];
            if row {
                b.x = cx + main_pos;
                b.y = cy + line_offsets[li] + cross_pos;
            } else {
                b.x = cx + line_offsets[li] + cross_pos;
                b.y = cy + main_pos;
            }
        }
    }

    // Final container height.
    let content_h = if row { content_cross } else { content_main };
    let mut h = match spec_h {
        Some(h) => h,
        None => content_h + l.vpad(id),
    };
    h = l.clamp_h(id, h, cb_h);
    l.boxes[id].h = h;
    l.finish_block_extras(id);
    if row {
        if let Some(&f) = lines.first().and_then(|ln| ln.first()) {
            let fb = items[f].id;
            let b = &l.boxes[fb];
            // No baseline in the first item: synthesize one from its border box bottom.
            let bl = l.first_baseline(fb).unwrap_or(b.h) + b.y;
            l.boxes[id].baseline = Some(bl);
        }
    }
}
