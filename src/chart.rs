//! 走勢圖（egui 版）：三個繪圖層，各自快取成 Mesh，只有需要時才重建——
//!
//! - plot：格線、軸、K 棒、20 組主圖疊加、≤10 副圖（tick／平移／縮放時重建）
//! - draw：上千筆畫線（只在尺度／內容變時重建；平移時頂點整批位移；不動就沿用同一個 Arc<Mesh>）
//! - live：圖例、十字線、選取／繪製中的畫線（滑鼠移動只重建這層）
//!
//! egui 每一幀都會把整個視窗重新送一次給 GPU（沒有局部重畫），所以「快取」省的是 CPU 端產生圖元／排字／三角化，
//! 不是光柵化。左側畫線工具列、上方週期／指標選單。XQ_CHART_STRESS=1 跑與 wry／GPUI 版相同的重圖表壓測。

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::epaint::Mesh;
use egui::{pos2, vec2, Color32, Context, Key, Rect, Sense, Shape, Ui};

use crate::drawings::{self, PaintCache, Tool};
use crate::layers::ZOp;
use crate::zstress::{ZMode, ZState};
use crate::gfx::{self, p, Align, Sink, P};
use crate::indicators::{OvSpec, SubKind, MAX_OVERLAYS, MAX_SUBS};
use crate::model::{DrawCache, IndCfg, Model, PaneKind, ALL, axis_w, DRAW, HEAD, LIVE, PLOT, SAVE, SEL, time_h};
use crate::series::Period;
use crate::store::{Entry, Store};
use crate::theme::*;

// 走勢圖配色：來自設計 token（color.chart.*）
pub fn c_up() -> u32 { tk().chart.up }
pub fn c_down() -> u32 { tk().chart.down }
fn c_grid() -> u32 { tk().chart.grid }
fn c_axis() -> u32 { tk().chart.axis }
fn c_text() -> u32 { tk().chart.text }
fn c_sep() -> u32 { tk().chart.separator }
fn c_cross() -> u32 { tk().chart.crosshair }
fn c_cross_label() -> u32 { tk().chart.crosshair_label }
pub fn ov_color(i: usize) -> u32 { let o = &tk().chart.overlay; o[i % o.len()] }
fn head_h() -> f32 { xq_tokens::t().layout.chart_header }
fn rail_w() -> f32 { xq_tokens::t().layout.tool_rail }

fn sub_colors(k: SubKind) -> &'static [u32] {
    let c = &tk().chart;
    match k {
        SubKind::Vol => &c.sub_vol,
        SubKind::Kd => &c.sub_kd,
        SubKind::Macd => &c.sub_macd,
        SubKind::Rsi => &c.sub_rsi,
        SubKind::Wr => &c.sub_wr,
        SubKind::Dmi => &c.sub_dmi,
        SubKind::Atr => &c.sub_atr,
        SubKind::Obv => &c.sub_obv,
    }
}

fn hex(c: u32) -> Color32 {
    rgb(c)
}
fn hexa(c: u32, a: f32) -> Color32 {
    rgba(c, a)
}
fn fmt_p(v: f64) -> String {
    if v.is_finite() { format!("{v:.2}") } else { "--".into() }
}
fn fmt_v(v: f64) -> String {
    if v.is_finite() { fmt_int(v.round() as i64) } else { "--".into() }
}
fn fmt_auto(v: f64) -> String {
    if !v.is_finite() { "--".into() } else if v.abs() >= 10000.0 { fmt_v(v) } else { format!("{v:.2}") }
}
fn nice_step(range: f64, count: f64) -> f64 {
    let raw = range / count.max(1.0);
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    (if n < 1.5 { 1.0 } else if n < 3.0 { 2.0 } else if n < 7.0 { 5.0 } else { 10.0 }) * mag
}
fn fmt_time(t: u32, period: Period, long: bool) -> String {
    match period {
        Period::Intraday => format!("{:02}:{:02}", t / 60, t % 60),
        Period::Monthly => format!("{}/{:02}", t / 10_000, t / 100 % 100),
        Period::Weekly => format!("{}/{:02}/{:02}", t / 10_000, t / 100 % 100, t % 100),
        Period::Daily if long => format!("{}/{:02}/{:02}", t / 10_000, t / 100 % 100, t % 100),
        Period::Daily => format!("{:02}/{:02}", t / 100 % 100, t % 100),
    }
}

fn clip_rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(w.max(0.0), h.max(0.0)))
}

fn line(s: &mut Sink, w: f32, c: Color32, i0: usize, i1: usize, x: impl Fn(usize) -> f32, y: impl Fn(f64) -> f32, v: &[f64]) {
    s.polyline(w, c, (i0..=i1).map(|i| v[i].is_finite().then(|| p(x(i), y(v[i])))));
}

type Layer = Vec<(Rect, Arc<Mesh>)>;

// ───────────────────────── plot 層 ─────────────────────────

fn build_plot(m: &mut Model, b: Rect, ctx: &Context) -> Layer {
    let g = m.geo(b).clone();
    let m = &*m;
    let bars = &m.s.bars;
    let (bx, by, bw) = (b.min.x, b.min.y, b.width());
    let mut glob = Sink::default();
    if bars.is_empty() {
        let msg = if m.s.period == Period::Intraday { "等待引擎分時資料…（需連上 WryFeedHost）" } else { "沒有資料" };
        glob.text(msg, bx + 16.0, by + 16.0, tk().font.md, hex(c_axis()));
        return vec![(b, Arc::new(glob.finish(ctx)))];
    }
    let intraday = m.s.period == Period::Intraday;
    let (i0, i1) = (g.i0, g.i1);
    let x = |i: usize| g.x(i as f64);
    let (up_c, dn_c) = (hex(c_up()), hex(c_down()));
    let axis = hex(c_axis());
    let axis_tx = g.plot_r + 6.0;
    let mut out: Layer = Vec::with_capacity(g.panes.len() + 1);
    for pane in &g.panes {
        let mut s = Sink::default();
        let (top, h) = (pane.y0, pane.ph);
        if pane.top > by {
            glob.rect(g.plot_l, pane.top, g.plot_w, 1.0, hex(c_sep()));
        }
        for &i in &g.ticks {
            let xx = x(i).round();
            if xx >= g.plot_l && xx <= g.plot_r {
                glob.rect(xx, top, 1.0, h, hex(c_grid()));
            }
        }
        let sub = match pane.kind {
            PaneKind::Price => None,
            PaneKind::Sub(k) => Some(&m.subs[k]),
        };
        if let Some((_, guides)) = sub.and_then(|s| s.fixed()) {
            for &t in guides {
                let y = pane.y(t).round();
                glob.hdash(g.plot_l, g.plot_r, y, hex(c_grid()), 3.0, 3.0);
                glob.text(format!("{t}"), axis_tx, y, tk().font.sm, axis);
            }
        } else {
            let st = nice_step(pane.hi - pane.lo, if sub.is_some() { (h / 30.0).max(1.0) as f64 } else { (h / 45.0).max(2.0) as f64 });
            let mut t = (pane.lo / st).ceil() * st;
            let mut guard = 0;
            while t <= pane.hi && guard < 50 {
                let y = pane.y(t).round();
                if y >= top + 4.0 && y <= top + h - 2.0 {
                    glob.rect(g.plot_l, y, g.plot_w, 1.0, hex(c_grid()));
                    let lbl = if t.abs() >= 10000.0 { fmt_v(t) } else if st < 1.0 { format!("{t:.2}") } else if st < 10.0 { format!("{t:.1}") } else { format!("{t:.0}") };
                    glob.text(lbl, axis_tx, y, tk().font.sm, axis);
                }
                t += st;
                guard += 1;
            }
            if sub.is_some_and(|s| s.cfg.kind == SubKind::Dmi) && 20.0 > pane.lo && 20.0 < pane.hi {
                glob.hdash(g.plot_l, g.plot_r, pane.y(20.0).round(), hex(tk().chart.midline), 3.0, 3.0);
            }
        }
        let yv = |v: f64| pane.y(v);
        match sub {
            None => {
                if intraday {
                    s.polyline(1.5, if m.change >= 0.0 { up_c } else { dn_c }, (i0..=i1).map(|i| Some(p(x(i), yv(bars[i].c)))));
                    if m.s.prev > 0.0 {
                        s.hdash(g.plot_l, g.plot_r, yv(m.s.prev), hex(tk().chart.prev_close), 4.0, 4.0);
                    }
                } else {
                    let bwid = (g.bar_w * 0.7).round().max(1.0) as f32;
                    for i in i0..=i1 {
                        let k = &bars[i];
                        let c = if k.c >= k.o { up_c } else { dn_c };
                        let xx = x(i).round();
                        let (yh, yl) = (yv(k.h), yv(k.l));
                        s.rect(xx, yh, 1.0, (yl - yh).max(1.0), c);
                        if g.bar_w >= 3.0 {
                            let (y1, y2) = (yv(k.o.max(k.c)), yv(k.o.min(k.c)));
                            s.rect(xx - (bwid / 2.0).floor() + 0.5, y1, bwid, (y2 - y1).max(1.0), c);
                        }
                    }
                }
                for (idx, o) in m.ovs.iter().enumerate() {
                    let col = hex(ov_color(idx));
                    match o.spec {
                        OvSpec::Sar { .. } => {
                            let r = (g.bar_w * 0.2).clamp(1.2, 2.5) as f32;
                            for i in i0..=i1 {
                                let v = o.sar_at(i);
                                if v.is_finite() {
                                    s.dot(x(i), yv(v), r, if o.out.sar_up[i] { up_c } else { dn_c });
                                }
                            }
                        }
                        OvSpec::Bb { .. } => {
                            let fc = hexa(tk().chart.area_fill.rgb, tk().chart.area_fill.a);
                            for i in i0..i1 {
                                let (u0, u1, l0, l1) = (o.out.up[i], o.out.up[i + 1], o.out.lo[i], o.out.lo[i + 1]);
                                if u0.is_finite() && u1.is_finite() && l0.is_finite() && l1.is_finite() {
                                    let (a, bq, c, d) = (p(x(i), yv(u0)), p(x(i + 1), yv(u1)), p(x(i + 1), yv(l1)), p(x(i), yv(l0)));
                                    s.tri(a, bq, c, fc);
                                    s.tri(a, c, d, fc);
                                }
                            }
                            line(&mut s, 1.2, col, i0, i1, x, yv, &o.out.up);
                            line(&mut s, 1.2, col, i0, i1, x, yv, &o.out.lo);
                            let mut prev: Option<P> = None;
                            for i in i0..=i1 {
                                let v = o.out.line[i];
                                let q = v.is_finite().then(|| p(x(i), yv(v)));
                                if let (Some(a), Some(c)) = (prev, q) {
                                    s.dashed(a, c, 1.0, col, 4.0, 3.0);
                                }
                                prev = q;
                            }
                        }
                        OvSpec::Ma { .. } | OvSpec::Ema { .. } => line(&mut s, 1.2, col, i0, i1, x, yv, &o.out.line),
                    }
                }
            }
            Some(sb) => {
                if sb.cfg.kind == SubKind::Vol {
                    let bw = (g.bar_w * 0.7).max(1.0) as f32;
                    let base = pane.y0 + pane.ph;
                    for i in i0..=i1 {
                        let k = &bars[i];
                        let up = if intraday { i == 0 || k.c >= bars[i - 1].c } else { k.c >= k.o };
                        let y = yv(k.v);
                        s.rect(x(i) - bw / 2.0, y, bw, base - y, hexa(if up { c_up() } else { c_down() }, 0xaa as f32 / 255.0));
                    }
                }
                if !sb.out.hist.is_empty() {
                    let bw = (g.bar_w * 0.6).max(1.0) as f32;
                    let y0 = yv(0.0);
                    for i in i0..=i1 {
                        let v = sb.out.hist[i];
                        if v.is_finite() {
                            let y = yv(v);
                            s.rect(x(i) - bw / 2.0, y.min(y0), bw, (y - y0).abs().max(0.5), if v >= 0.0 { up_c } else { dn_c });
                        }
                    }
                }
                let cols = sub_colors(sb.cfg.kind);
                for (k, l) in sb.out.lines.iter().enumerate() {
                    line(&mut s, 1.2, hex(cols[k % cols.len()]), i0, i1, x, yv, l);
                }
            }
        }
        out.push((clip_rect(g.plot_l, pane.top + 1.0, g.plot_w, pane.h - 1.0), Arc::new(s.finish(ctx))));
    }
    glob.rect(bx, g.bottom, bw, 1.0, hex(c_sep()));
    glob.rect(g.plot_r, by, 1.0, g.bottom - by, hex(c_sep()));
    for &i in &g.ticks {
        let st = fmt_time(bars[i].t, m.s.period, false);
        let half = drawings::text_width(&st, 11.0) / 2.0 + 2.0;
        let cxp = x(i).clamp(g.plot_l + half, g.plot_r - half);
        glob.text_at(st, cxp, g.bottom + time_h() / 2.0 + 1.0, tk().font.sm, axis, Align::Center);
    }
    let last = bars[bars.len() - 1];
    let pr = g.panes[0];
    let ly = pr.y(last.c);
    if ly >= pr.y0 && ly <= pr.y0 + pr.ph {
        let c = if m.change > 0.0 { c_up() } else if m.change < 0.0 { c_down() } else { tk().chart.flat };
        glob.rect(g.plot_r + 1.0, ly - 8.0, axis_w() - 2.0, 16.0, hex(c));
        glob.text(fmt_p(last.c), g.plot_r + 5.0, ly, tk().font.sm, hex(tk().chart.tag_text));
    }
    // 格線／軸在最底下：把 glob 放在最前面
    out.insert(0, (b, Arc::new(glob.finish(ctx))));
    out
}

// ───────────────────────── live 層 ─────────────────────────

fn build_live(m: &mut Model, b: Rect, ctx: &Context) -> Layer {
    let g = m.geo(b).clone();
    let m = &*m;
    let bars = &m.s.bars;
    if bars.is_empty() {
        return Vec::new();
    }
    let xf = g.xf();
    let mut out: Layer = Vec::new();
    let mut sel = Sink::default();
    let mut hs = Vec::new();
    // 繪製中的那條（預覽，畫在最上面）
    if let Some(pd) = &m.pending {
        let g = drawings::geom(&pd.d, &xf);
        hs.extend_from_slice(&g.handles);
        drawings::emit(&mut sel, &pd.d, &g, false, 0.0);
    }
    // 選取那條的控制點永遠在最上面（線本身在「選取那片」、原本的順位）
    if let Some(d) = m.selected.and_then(|k| m.drawings.get(k)) {
        hs.extend_from_slice(&drawings::geom(d, &xf).handles);
    }
    drawings::emit_handles(&mut sel, &hs);
    if !sel.is_empty() {
        out.push((xf.clip(), Arc::new(sel.finish(ctx))));
    }
    let mut s = Sink::default();
    if let Some(d) = m.selected.and_then(|k| m.drawings.get(k)).filter(|d| d.kind == Tool::Hline) {
        let y = xf.y(d.pts[0].p);
        if y > xf.pane_top + 8.0 && y < xf.pane_top + xf.pane_h - 8.0 {
            s.rect(g.plot_r + 1.0, y - 8.0, axis_w() - 2.0, 16.0, hex(tk().chart.tag_bg));
            s.text(fmt_p(d.pts[0].p), g.plot_r + 5.0, y, tk().font.sm, hex(tk().chart.tag_text));
        }
    }
    if let Some(te) = &m.text_edit {
        let st = format!("{}▏", te.text);
        let w = drawings::text_width(&st, 13.0).max(120.0) + 12.0;
        let (x0, y0) = (te.at.x, te.at.y - 11.0);
        s.boxed(x0, y0, w, 22.0, hexa(tk().chart.tooltip_bg.rgb, tk().chart.tooltip_bg.a), hex(tk().chart.tooltip_border), 1.0);
        s.text(st, x0 + 6.0, y0 + 12.0, tk().font.lg, hex(drawings::draw_color()));
        s.text("輸入文字，Enter 確定，Esc 取消", x0, y0 + 32.0, tk().font.sm, hex(c_axis()));
    }
    let hov = g.hover_idx(m.mouse);
    let info = hov.unwrap_or(g.i1.min(bars.len() - 1));
    if let (Some(i), Some(q)) = (hov, m.mouse) {
        let xx = g.x(i as f64).round();
        let y = q.y.round();
        s.vdash(xx, b.min.y, g.bottom, hex(c_cross()), 3.0, 3.0);
        s.hdash(g.plot_l, g.plot_r, y, hex(c_cross()), 3.0, 3.0);
        let pane = g.pane_at(q).map(|k| g.panes[k]).unwrap_or(g.panes[0]);
        let v = pane.val_at(q.y);
        let is_vol = matches!(pane.kind, PaneKind::Sub(k) if matches!(m.subs[k].cfg.kind, SubKind::Vol | SubKind::Obv));
        s.rect(g.plot_r + 1.0, y - 8.0, axis_w() - 2.0, 16.0, hex(c_cross_label()));
        s.text(if is_vol { fmt_v(v) } else { fmt_auto(v) }, g.plot_r + 5.0, y, tk().font.sm, hex(tk().chart.tag_text));
        let st = fmt_time(bars[i].t, m.s.period, true);
        let w = drawings::text_width(&st, 11.0) + 10.0;
        let bxl = (xx - w / 2.0).clamp(g.plot_l, g.plot_r - w);
        s.rect(bxl, g.bottom + 1.0, w, time_h() - 2.0, hex(c_cross_label()));
        s.text_at(st, bxl + w / 2.0, g.bottom + time_h() / 2.0 + 1.0, tk().font.sm, hex(tk().chart.tag_text), Align::Center);
    }
    out.push((b, Arc::new(s.finish(ctx))));
    // 圖例（剪裁在各窗格內）
    let k = bars[info];
    let prev = if m.s.period == Period::Intraday { m.s.prev } else if info > 0 { bars[info - 1].c } else { k.o };
    let ch = k.c - prev;
    let col = hex(if ch >= 0.0 { c_up() } else { c_down() });
    let (t_c, a_c) = (hex(c_text()), hex(c_axis()));
    let src = if m.s.live { "engine" } else if m.anchored { "合成K·現價" } else { "合成" };
    let sg = if ch >= 0.0 { "+" } else { "" };
    let head: Vec<(String, Color32)> = vec![
        (format!("{}（{}）", m.s.period.label(), src), a_c),
        (fmt_time(k.t, m.s.period, true), t_c),
        (format!("開 {}", fmt_p(k.o)), t_c),
        (format!("高 {}", fmt_p(k.h)), t_c),
        (format!("低 {}", fmt_p(k.l)), t_c),
        (format!("收 {}", fmt_p(k.c)), col),
        (format!("{sg}{ch:.2} ({sg}{:.2}%)", if prev != 0.0 { ch / prev * 100.0 } else { 0.0 }), col),
        (format!("量 {}", fmt_v(k.v)), t_c),
    ];
    let pr = g.panes[0];
    let lx = g.plot_l + 4.0;
    let mut leg = Sink::default();
    items_line(&mut leg, &head, lx, pr.top + 8.0);
    let mut items: Vec<(String, Color32)> = Vec::with_capacity(32);
    for (idx, o) in m.ovs.iter().enumerate() {
        let c = hex(ov_color(idx));
        match o.spec {
            OvSpec::Ma { period } => items.push((format!("MA{period} {}", fmt_p(o.out.line[info])), c)),
            OvSpec::Ema { period } => items.push((format!("EMA{period} {}", fmt_p(o.out.line[info])), c)),
            OvSpec::Bb { n, k } => {
                items.push((format!("BB({n},{k}) {}", fmt_p(o.out.line[info])), c));
                items.push((format!("上 {}", fmt_p(o.out.up[info])), c));
                items.push((format!("下 {}", fmt_p(o.out.lo[info])), c));
            }
            OvSpec::Sar { .. } => items.push((format!("SAR {}", fmt_p(o.sar_at(info))), c)),
        }
    }
    let max_w = g.plot_w - 8.0;
    let mut lines: Vec<Vec<(String, Color32)>> = vec![Vec::new()];
    let mut wsum = 0.0;
    for it in items {
        let w = drawings::text_width(&it.0, 11.0) + 10.0;
        if wsum + w > max_w && !lines.last().unwrap().is_empty() {
            lines.push(Vec::new());
            wsum = 0.0;
        }
        wsum += w;
        lines.last_mut().unwrap().push(it);
    }
    let mut y = pr.top + 8.0;
    for ln in lines.iter().filter(|l| !l.is_empty()) {
        y += 14.0;
        items_line(&mut leg, ln, lx, y);
    }
    out.push((clip_rect(g.plot_l, pr.top, g.plot_w, pr.h), Arc::new(leg.finish(ctx))));
    for pane in &g.panes[1..] {
        let PaneKind::Sub(si) = pane.kind else { continue };
        let sb = &m.subs[si];
        let is_vol = sb.cfg.kind == SubKind::Vol;
        let mut it = vec![(sb.title(), a_c)];
        if is_vol {
            it.push((fmt_v(k.v), t_c));
        }
        let names = sb.line_names();
        let cols = sub_colors(sb.cfg.kind);
        for (li, l) in sb.out.lines.iter().enumerate() {
            let nm = if is_vol { format!("MA{}", sb.volma.map_or(0, |v| v[li])) } else { names.get(li).copied().unwrap_or("").to_string() };
            let v = if is_vol { fmt_v(l[info]) } else { fmt_auto(l[info]) };
            it.push((if nm.is_empty() { v } else { format!("{nm} {v}") }, hex(cols[li % cols.len()])));
        }
        if let Some(&hv) = sb.out.hist.get(info) {
            it.push((format!("OSC {}", fmt_auto(hv)), hex(if hv >= 0.0 { c_up() } else { c_down() })));
        }
        let mut ls = Sink::default();
        items_line(&mut ls, &it, lx, pane.top + 7.0);
        out.push((clip_rect(g.plot_l, pane.top, g.plot_w, pane.h), Arc::new(ls.finish(ctx))));
    }
    out
}

/// 多色一行（圖例）：項目之間空 2 格（等寬字寬估計，避免為了量字寬先排一次字）
fn items_line(s: &mut Sink, items: &[(String, Color32)], x: f32, y: f32) {
    let mut xx = x;
    for (t, c) in items {
        s.text(t.clone(), xx, y, tk().font.sm, *c);
        xx += egui_text_w(t, 11.0) + 2.0 * 0.6 * 11.0;
    }
}

/// Noto Sans Mono CJK：ASCII 半形 0.5em、CJK 全形 1em
fn egui_text_w(s: &str, size: f32) -> f32 {
    s.chars().map(|c| if c.is_ascii() { 0.5 } else { 1.0 }).sum::<f32>() * size
}

// ───────────────────────── 壓測驅動（XQ_CHART_STRESS） ─────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Ticks,
    Crosshair,
    Pan,
    /// 圖層／上下順序四段（zstress.rs）
    Z(ZMode),
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Ticks => "ticks",
            Mode::Crosshair => "crosshair",
            Mode::Pan => "pan",
            Mode::Z(z) => z.name(),
        }
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct LayerStats {
    /// 0 底圖、1 畫線（下段＋上段）、2 live、3 選取那片
    pub paints: [u64; 4],
    pub paint_us: [u64; 4],
    pub rebuilds: u64,
    pub rebuild_us: u64,
}

#[derive(Clone, Copy)]
struct Snap {
    frames: u64,
    st: LayerStats,
    t: Instant,
}

enum Stage {
    WaitQuote(Instant),
    Delay(Instant),
    HeavyPending,
    Warm(Instant),
    Run { mode: Mode, s0: Snap, steps: u64, base_right: f64 },
    Done,
}

struct StressCfg {
    drawings: usize,
    layers: usize,
    warm: Duration,
    phase: Duration,
    wait: Duration,
    t0: Instant,
}

fn env_ms(k: &str, d: u64) -> Duration {
    Duration::from_millis(std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d))
}

// ───────────────────────── ChartView ─────────────────────────

pub enum ChartEvent {
    Period(Period),
}

#[derive(Clone, Copy, PartialEq)]
struct LayerKey {
    rect: Rect,
    font: ([usize; 2], u32),
}

pub struct ChartView {
    pub m: Model,
    /// 待重建的層（PLOT／DRAW／LIVE 旗標，與 gpui 版 refresh 同義）
    dirty: u8,
    plot: Option<(LayerKey, Layer)>,
    live: Option<(LayerKey, Layer)>,
    /// 三明治中間那片：選取的那條（拖曳時每幀只重建這片＋live）
    sel: Option<(LayerKey, Layer)>,
    pub stats: LayerStats,
    menu_open: bool,
    layers_open: bool,
    store: Store,
    persist: bool,
    save_at: Option<Instant>,
    stress: Option<(StressCfg, Stage)>,
    zs: ZState,
    total_steps: u64,
    focused: bool,
    last_press: Option<(Instant, egui::Pos2)>,
    last_pointer: Option<egui::Pos2>,
    /// 走勢圖繪圖區（不含工具列與標頭）
    pub stage: Option<Rect>,
    draw_font: Option<([usize; 2], u32)>,
}

impl ChartView {
    pub fn new(symbol: &str, name: &str, period: Period) -> Self {
        let stress_on = std::env::var_os("XQ_CHART_STRESS").is_some_and(|v| v != "0");
        let persist = !stress_on && !std::env::var_os("XQ_BENCH").is_some_and(|v| v != "0");
        let store = if persist { Store::load() } else { Store::default() };
        let stress = stress_on.then(|| {
            let now = Instant::now();
            (
                StressCfg {
                    drawings: std::env::var("XQ_CHART_DRAWINGS").ok().and_then(|v| v.parse().ok()).unwrap_or(1000),
                    layers: crate::zstress::env_layers(),
                    warm: env_ms("XQ_CHART_WARM_MS", 3000),
                    phase: env_ms("XQ_CHART_PHASE_MS", 10_000),
                    wait: env_ms("XQ_CHART_WAIT_MS", 15_000),
                    t0: now,
                },
                Stage::WaitQuote(now),
            )
        });
        let mut v = ChartView {
            m: Model::new(symbol, name, period),
            dirty: ALL,
            plot: None,
            live: None,
            sel: None,
            stats: LayerStats::default(),
            menu_open: false,
            layers_open: false,
            store,
            persist,
            save_at: None,
            stress,
            zs: ZState::new(crate::zstress::env_layers()),
            total_steps: 0,
            focused: false,
            last_press: None,
            last_pointer: None,
            stage: None,
            draw_font: None,
        };
        v.restore_saved();
        v
    }

    pub fn symbol(&self) -> String {
        self.m.symbol().to_string()
    }
    pub fn period(&self) -> Period {
        self.m.period()
    }
    pub fn stress_active(&self) -> bool {
        self.stress.as_ref().is_some_and(|(_, s)| !matches!(s, Stage::Done))
    }
    pub fn paint_totals(&self) -> (u64, u64) {
        (self.stats.paints.iter().sum(), self.stats.paint_us.iter().sum())
    }

    /// 換主題／token 熱更新：丟掉烤了顏色的畫線快取，三層都重建
    pub fn tokens_changed(&mut self) {
        self.m.pc = None;
        self.refresh(ALL);
    }

    fn refresh(&mut self, f: u8) {
        self.dirty |= f & (PLOT | DRAW | LIVE | SEL);
        if f & SAVE != 0 {
            self.schedule_save();
        }
    }

    // ───── 存檔 ─────

    fn restore_saved(&mut self) {
        if !self.persist {
            return;
        }
        let key = self.m.key();
        let e = self.store.keys.get(&key).cloned().unwrap_or_default();
        let cfg = e.cfg.or_else(|| self.store.last_cfg.clone());
        if let Some(c) = cfg {
            if c != self.m.cfg {
                self.m.set_cfg(c);
            }
        }
        self.m.set_drawings(e.drawings, e.layers);
    }

    fn schedule_save(&mut self) {
        if !self.persist {
            return;
        }
        let key = self.m.key();
        self.store.v = 1;
        self.store.last_cfg = Some(self.m.cfg.clone());
        self.store.keys.insert(key, Entry { cfg: Some(self.m.cfg.clone()), drawings: self.m.drawings.clone(), layers: self.m.layers.clone() });
        self.save_at = Some(Instant::now() + Duration::from_millis(450));
    }

    // ───── 資料 ─────

    pub fn load(&mut self, symbol: &str, name: &str, period: Period, quote: Option<(f64, f64, i64)>) {
        self.m.load(symbol, name, period, quote);
        self.restore_saved();
        self.refresh(ALL);
    }

    pub fn apply_quote(&mut self, price: f64, change: f64, volume: i64, name: &str) {
        let f = self.m.apply_quote(price, change, volume, name);
        self.refresh(f);
    }

    pub fn set_intraday(&mut self, symbol: &str, prev: f64, bars: &[xq_feed::Bar]) {
        if self.m.symbol() != symbol || self.m.period() != Period::Intraday {
            return;
        }
        let f = self.m.set_intraday(prev, bars);
        self.refresh(f);
    }

    pub fn apply_minute(&mut self, symbol: &str, bar: &xq_feed::Bar) {
        if self.m.symbol() != symbol || self.m.period() != Period::Intraday {
            return;
        }
        let f = self.m.apply_minute(bar);
        self.refresh(f);
    }

    fn change_cfg(&mut self, f: impl FnOnce(&mut IndCfg)) {
        let mut c = self.m.cfg.clone();
        f(&mut c);
        c.overlays.truncate(MAX_OVERLAYS);
        if c != self.m.cfg {
            self.m.set_cfg(c);
        }
        self.refresh(ALL | SAVE);
    }

    // ───── 壓測 ─────

    fn snap(&self, frames: u64) -> Snap {
        Snap { frames, st: self.stats, t: Instant::now() }
    }

    /// 每一幀開始時呼叫（壓測期間主程式連續要求重畫）
    pub fn stress_frame(&mut self, frames: u64) {
        let Some((cfg, stage)) = self.stress.take() else { return };
        let now = Instant::now();
        let next = match stage {
            Stage::WaitQuote(t) => {
                let has = self.m.anchored;
                if has || now.duration_since(t) >= cfg.wait {
                    if !has {
                        eprintln!("CHARTPERF|{{\"phase\":\"warn\",\"msg\":\"no quote after {} ms, using synthetic bars\"}}", cfg.wait.as_millis());
                    }
                    Stage::Delay(now)
                } else {
                    Stage::WaitQuote(t)
                }
            }
            Stage::Delay(t) if now.duration_since(t) >= Duration::from_millis(300) => {
                let t_setup = Instant::now();
                self.m.set_cfg(IndCfg::stress());
                self.zs.pick = self.m.load_perf_drawings(cfg.drawings, cfg.layers);
                self.m.mouse = None;
                let setup_ms = t_setup.elapsed().as_secs_f64() * 1000.0;
                eprintln!("CHARTPERF|{{\"phase\":\"setup-begin\",\"indicatorSetupMs\":{setup_ms:.2}}}");
                self.stats = LayerStats::default();
                self.refresh(ALL);
                Stage::HeavyPending
            }
            s @ Stage::Delay(_) => s,
            Stage::HeavyPending => {
                eprintln!("BENCH heavy-frame");
                let b = self.m.bounds.unwrap_or(Rect::NOTHING);
                let st = self.stats;
                let vis = self.m.gc.as_ref().map_or(0, |g| g.visible);
                let verts = self.m.pc.as_ref().map_or(0, |c| c.below.vertex_count() + c.above.vertex_count());
                let first_paint_ms = st.paint_us.iter().sum::<u64>() as f64 / 1000.0;
                eprintln!(
                    "CHARTPERF|{{\"phase\":\"setup\",\"cssW\":{:.0},\"cssH\":{:.0},\"overlays\":{},\"subs\":{},\"drawings\":{},\"layers\":{},\"visibleDrawings\":{},\"drawVertices\":{},\"bars\":{},\"sinceStartMs\":{:.0},\"rebuildMs\":{:.2},\"paintMsTotal\":{:.2},\"rssMb\":{:.1}}}",
                    b.width(),
                    b.height(),
                    self.m.ovs.len(),
                    self.m.subs.len(),
                    self.m.drawings.len(),
                    self.m.layers.len(),
                    vis,
                    verts,
                    self.m.s.bars.len(),
                    cfg.t0.elapsed().as_secs_f64() * 1000.0,
                    st.rebuild_us as f64 / 1000.0,
                    first_paint_ms,
                    crate::rss_mb()
                );
                eprintln!("CHARTPHASE|warm|start");
                Stage::Warm(now)
            }
            Stage::Warm(t) => {
                let f = self.stress_step(Mode::Ticks, self.total_steps, 0.0);
                self.refresh(f);
                if now.duration_since(t) >= cfg.warm {
                    self.begin_phase(Mode::Ticks, frames)
                } else {
                    Stage::Warm(t)
                }
            }
            Stage::Run { mode, s0, steps, base_right } => {
                if let Mode::Z(_) = mode {
                    self.zs.record(self.stats.rebuild_us);
                }
                let dur = if mode == Mode::Z(ZMode::ZShot) { Duration::from_millis(3000) } else { cfg.phase };
                if now.duration_since(s0.t) >= dur {
                    let s1 = self.snap(frames);
                    let secs = s1.t.duration_since(s0.t).as_secs_f64();
                    let nfr = s1.frames - s0.frames;
                    let lp: Vec<u64> = (0..4).map(|k| s1.st.paints[k] - s0.st.paints[k]).collect();
                    let lu: Vec<u64> = (0..4).map(|k| s1.st.paint_us[k] - s0.st.paint_us[k]).collect();
                    let tot_us: u64 = lu.iter().sum();
                    let per = |k: usize| if lp[k] > 0 { lu[k] as f64 / lp[k] as f64 / 1000.0 } else { 0.0 };
                    eprintln!(
                        "CHARTPERF|{{\"phase\":\"{}\",\"seconds\":{:.2},\"frames\":{},\"fps\":{:.1},\"steps\":{},\"avgPaintMs\":{:.3},\"plotPaints\":{},\"plotMs\":{:.3},\"drawPaints\":{},\"drawMs\":{:.3},\"livePaints\":{},\"liveMs\":{:.3},\"selPaints\":{},\"selMs\":{:.3},\"drawRebuilds\":{},\"rebuildMsTotal\":{:.2},\"rssMb\":{:.1}{}}}",
                        mode.name(),
                        secs,
                        nfr,
                        nfr as f64 / secs,
                        steps,
                        if nfr > 0 { tot_us as f64 / nfr as f64 / 1000.0 } else { 0.0 },
                        lp[0],
                        per(0),
                        lp[1],
                        per(1),
                        lp[2],
                        per(2),
                        lp[3],
                        per(3),
                        s1.st.rebuilds - s0.st.rebuilds,
                        (s1.st.rebuild_us - s0.st.rebuild_us) as f64 / 1000.0,
                        crate::rss_mb(),
                        if let Mode::Z(_) = mode { self.zs.report(&self.m) } else { String::new() }
                    );
                    eprintln!("CHARTPHASE|{}|end", mode.name());
                    if mode == Mode::Pan {
                        self.m.set_right(base_right);
                        self.m.set_busy(false);
                        self.refresh(ALL);
                    }
                    if mode == Mode::Crosshair {
                        let f = self.m.mouse_move(p(-1.0, -1.0), false);
                        self.refresh(f);
                    }
                    if let Mode::Z(z) = mode {
                        let f = self.zs.end(&mut self.m, z);
                        self.refresh(f & !HEAD);
                    }
                    match mode {
                        Mode::Ticks => self.begin_phase(Mode::Crosshair, frames),
                        Mode::Crosshair => self.begin_phase(Mode::Pan, frames),
                        Mode::Pan => self.begin_phase(Mode::Z(ZMode::Drag), frames),
                        Mode::Z(ZMode::Drag) => self.begin_phase(Mode::Z(ZMode::Select), frames),
                        Mode::Z(ZMode::Select) => self.begin_phase(Mode::Z(ZMode::Reorder), frames),
                        Mode::Z(ZMode::Reorder) => self.begin_phase(Mode::Z(ZMode::ZShot), frames),
                        Mode::Z(ZMode::ZShot) => {
                            eprintln!("CHARTPERF|{{\"phase\":\"done\",\"sinceStartMs\":{:.0}}}", cfg.t0.elapsed().as_secs_f64() * 1000.0);
                            Stage::Done
                        }
                    }
                } else {
                    let f = self.stress_step(mode, steps, base_right);
                    self.refresh(f);
                    Stage::Run { mode, s0, steps: steps + 1, base_right }
                }
            }
            Stage::Done => {
                let f = self.stress_step(Mode::Ticks, self.total_steps, 0.0);
                self.refresh(f);
                Stage::Done
            }
        };
        self.stress = Some((cfg, next));
    }

    pub fn stress_on(&self) -> bool {
        self.stress.is_some()
    }

    fn begin_phase(&mut self, mode: Mode, frames: u64) -> Stage {
        if let Mode::Z(z) = mode {
            let f = self.zs.begin(&mut self.m, z);
            self.refresh(f & !HEAD);
        }
        eprintln!("CHARTPHASE|{}|start", mode.name());
        if mode == Mode::Pan {
            self.m.set_busy(true);
        }
        let base_right = self.m.view.map_or(0.0, |v| v.right);
        Stage::Run { mode, s0: self.snap(frames), steps: 0, base_right }
    }

    fn stress_step(&mut self, mode: Mode, f: u64, base_right: f64) -> u8 {
        self.total_steps += 1;
        if let Mode::Z(z) = mode {
            let fl = self.zs.step(&mut self.m, z, f, self.stats.rebuild_us);
            return fl & !HEAD;
        }
        let m = &mut self.m;
        let mut fl = m.synth_tick(f);
        if matches!(mode, Mode::Crosshair | Mode::Pan) {
            if let Some(b) = m.bounds {
                let (w, h) = (b.width(), b.height());
                let x = 80.0 + ((f * 13) as f32) % (w - 160.0).max(40.0);
                let y = 40.0 + ((f * 7) as f32) % (h * 0.5).max(40.0);
                fl |= m.mouse_move(p(b.min.x + x, b.min.y + y), true);
            }
        }
        if mode == Mode::Pan {
            m.set_right(base_right + (f as f64 / 6.0).sin() * 12.0);
            fl |= PLOT | DRAW | LIVE;
        }
        fl & !HEAD
    }
}

// ───────────────────────── 版面、事件、繪製 ─────────────────────────

impl ChartView {
    /// 畫在 rect 裡（含標頭、工具列、繪圖區）；回傳要切週期的事件
    pub fn show(&mut self, ui: &mut Ui, rect: Rect) -> Option<ChartEvent> {
        let ctx = ui.ctx().clone();
        let mut ev = None;
        if let Some(t) = self.save_at {
            if Instant::now() >= t {
                self.save_at = None;
                self.store.save_async();
            } else {
                ctx.request_repaint_after((t - Instant::now()).max(std::time::Duration::from_micros(1)));
            }
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, bg());
        let head = Rect::from_min_size(rect.min, vec2(rect.width(), head_h()));
        let rail = Rect::from_min_max(pos2(rect.min.x, head.max.y), pos2(rect.min.x + rail_w(), rect.max.y));
        let stage = Rect::from_min_max(pos2(rail.max.x, head.max.y), rect.max);
        self.stage = Some(stage);

        // ── 標頭 ──
        painter.rect_filled(head, 0.0, header());
        painter.hline(head.x_range(), head.max.y - 0.5, (1.0, border()));
        let mut hu = ui.new_child(egui::UiBuilder::new().max_rect(head.shrink2(vec2(8.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        hu.spacing_mut().item_spacing.x = 10.0;
        let (price, change, volume) = (self.m.price, self.m.change, self.m.volume);
        let color = dir_color(change);
        let has = price > 0.0;
        hu.label(egui::RichText::new(format!("{} {}", self.m.symbol(), self.m.name)).size(tk().font.lg).color(text()));
        let mono = |s: String, c: Color32| egui::RichText::new(s).monospace().size(tk().font.md).color(c);
        hu.label(mono(if has { format!("{price:.2}") } else { "--".into() }, color));
        if has {
            hu.label(mono(format!("{} ({:+.2}%)", fmt_change(change), pct(price, change)), color));
            hu.label(egui::RichText::new(format!("量 {}", fmt_int(volume))).size(tk().font.md).color(dim()));
        }
        let n_draw = self.m.drawings.len();
        let hint = match self.m.tool {
            Some(Tool::Text) => "點一下放文字".to_string(),
            Some(Tool::Channel) => "拖出基準線，再點一下決定通道寬度".to_string(),
            Some(t) if t.need() == 1 => format!("{}：點一下放置", t.label()),
            Some(t) => format!("{}：按住拖曳（或點兩下）", t.label()),
            None if n_draw > 0 => format!("畫線 {n_draw}"),
            None => String::new(),
        };
        hu.label(egui::RichText::new(hint).size(tk().font.sm).color(dim()));
        hu.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if small_btn(ui, "重設縮放", false).clicked() {
                let f = self.m.reset_view();
                self.refresh(f);
            }
            let lb = small_btn(ui, "圖層    ", self.layers_open);
            let c = lb.rect.right_center() + vec2(-10.0, 0.0);
            ui.painter().add(Shape::convex_polygon(
                vec![c + vec2(-3.5, -2.0), c + vec2(3.5, -2.0), c + vec2(0.0, 2.5)],
                if self.layers_open { text() } else { dim() },
                egui::Stroke::NONE,
            ));
            if lb.clicked() {
                self.layers_open = !self.layers_open;
            }
            // Noto Sans CJK 沒有 ▾（U+25BE）字形，會畫成缺字方塊：字後留空，三角形自己畫（與 gpui 版外觀一致）
            let mb = small_btn(ui, "指標    ", self.menu_open);
            let c = mb.rect.right_center() + vec2(-10.0, 0.0);
            ui.painter().add(Shape::convex_polygon(
                vec![c + vec2(-3.5, -2.0), c + vec2(3.5, -2.0), c + vec2(0.0, 2.5)],
                if self.menu_open { text() } else { dim() },
                egui::Stroke::NONE,
            ));
            if mb.clicked() {
                self.menu_open = !self.menu_open;
            }
            for &pd in Period::ALL.iter().rev() {
                if small_btn(ui, pd.label(), pd == self.m.period()).clicked() {
                    ev = Some(ChartEvent::Period(pd));
                }
            }
        });

        // ── 工具列 ──
        self.show_rail(ui, rail);

        // ── 指標選單（浮在最上層） ──
        let mut menu_rect = None;
        if self.menu_open {
            menu_rect = self.show_menu(&ctx, pos2(rect.max.x - 428.0, rect.min.y + 28.0));
        }
        if self.layers_open {
            let r = self.show_layers(&ctx, pos2(rect.max.x - 358.0, rect.min.y + 28.0));
            menu_rect = Some(match menu_rect {
                Some(m) => m.union(r),
                None => r,
            });
        }

        // ── 繪圖區輸入 ──
        self.handle_input(ui, stage, menu_rect);

        // ── 三層 ──
        let font = gfx::font_key(&ctx);
        let key = LayerKey { rect: stage, font };
        if gfx::no_view_cache() {
            self.dirty |= PLOT | LIVE;
        }
        let layer_painter = |n: u8| {
            if gfx::split_layers() {
                let id = egui::LayerId::new(ui.layer_id().order, ui.scope_id().with(("chart-layer", n)));
                ctx.set_sublayer(ui.layer_id(), id);
                ctx.layer_painter(id).with_clip_rect(stage)
            } else {
                ui.painter_at(stage)
            }
        };
        let sp = layer_painter(0);
        if self.dirty & PLOT != 0 || !matches!(&self.plot, Some((k, _)) if *k == key) {
            let t = Instant::now();
            let layer = build_plot(&mut self.m, stage, &ctx);
            self.plot = Some((key, layer));
            self.stats.paints[0] += 1;
            self.stats.paint_us[0] += t.elapsed().as_micros() as u64;
            // 資料變了，圖例也要跟著變
            self.dirty |= LIVE;
        }
        for (clip, mesh) in &self.plot.as_ref().unwrap().1 {
            sp.with_clip_rect(*clip).add(Shape::Mesh(mesh.clone()));
        }
        let dp = layer_painter(1);
        self.paint_draw(&ctx, &dp, stage);
        let sp = layer_painter(2);
        if self.dirty & LIVE != 0 || !matches!(&self.live, Some((k, _)) if *k == key) {
            let t = Instant::now();
            let layer = build_live(&mut self.m, stage, &ctx);
            self.live = Some((key, layer));
            self.stats.paints[2] += 1;
            self.stats.paint_us[2] += t.elapsed().as_micros() as u64;
        }
        for (clip, mesh) in &self.live.as_ref().unwrap().1 {
            sp.with_clip_rect(*clip).add(Shape::Mesh(mesh.clone()));
        }
        self.dirty = 0;
        ev
    }

    /// 畫線層三明治：下段（快取）→ 選取那條（原本順位，即時）→ 上段（快取）。
    /// 下段＋上段只在尺度、畫線內容、圖層或選取變了才重建（只建可見的）；拖曳選取那條時只重建中間那片。
    fn paint_draw(&mut self, ctx: &Context, sp: &egui::Painter, stage: Rect) {
        let m = &mut self.m;
        m.geo(stage);
        if m.drawings.is_empty() {
            m.pc = None;
            self.sel = None;
            return;
        }
        let dx = m.ensure_gc();
        let Some((key, right, _)) = m.draw_key() else { return };
        let xf = m.geo_now().unwrap().xf();
        let font = gfx::font_key(ctx);
        if self.draw_font != Some(font) {
            // 字型貼圖長大：舊 Mesh 的字 UV 失效
            self.draw_font = Some(font);
            m.pc = None;
        }
        let stale = !matches!(&m.pc, Some(c) if c.key == key);
        if stale {
            let t = Instant::now();
            let ord = m.ensure_order().clone();
            let (lo, _, hi) = ord.split(m.selected);
            let gc = m.gc.as_ref().unwrap();
            let mut bxf = xf;
            bxf.right = gc.right;
            let below = PaintCache::build(ctx, &m.drawings, gc, &bxf, lo);
            let above = PaintCache::build(ctx, &m.drawings, gc, &bxf, hi);
            self.stats.rebuilds += 1;
            let us = t.elapsed().as_micros() as u64;
            self.stats.rebuild_us += us;
            self.stats.paints[1] += 1;
            self.stats.paint_us[1] += us;
            m.pc = Some(DrawCache { key, right, below, above });
        }
        let lkey = LayerKey { rect: stage, font };
        if self.dirty & (SEL | DRAW | PLOT) != 0 || stale || !matches!(&self.sel, Some((k, _)) if *k == lkey) {
            let t = Instant::now();
            let mut layer = Layer::new();
            if let Some(k) = m.selected.filter(|&k| !m.ensure_order().hidden(k)) {
                if let Some(d) = m.drawings.get(k) {
                    let mut s = gfx::Sink::default();
                    drawings::emit(&mut s, d, &drawings::geom(d, &xf), true, 0.0);
                    layer.push((xf.clip(), Arc::new(s.finish(ctx))));
                }
            }
            self.sel = Some((lkey, layer));
            self.stats.paints[3] += 1;
            self.stats.paint_us[3] += t.elapsed().as_micros() as u64;
        }
        let c = m.pc.as_mut().unwrap();
        c.right = right;
        let t = Instant::now();
        let clip = xf.clip();
        let main = c.below.main_at(dx);
        sp.with_clip_rect(clip).add(Shape::Mesh(main));
        sp.add(Shape::Mesh(c.below.tags.clone()));
        for (cl, mesh) in &self.sel.as_ref().unwrap().1 {
            sp.with_clip_rect(*cl).add(Shape::Mesh(mesh.clone()));
        }
        if m.selected.is_some() {
            let main = c.above.main_at(dx);
            sp.with_clip_rect(clip).add(Shape::Mesh(main));
            sp.add(Shape::Mesh(c.above.tags.clone()));
        }
        if dx != 0.0 && !stale {
            // 平移位移也算一次畫線層更新
            self.stats.paints[1] += 1;
            self.stats.paint_us[1] += t.elapsed().as_micros() as u64;
        }
    }

    fn handle_input(&mut self, ui: &mut Ui, stage: Rect, menu: Option<Rect>) {
        let resp = ui.interact(stage, ui.scope_id().with("chart-stage"), Sense::click_and_drag());
        let (pressed, released, latest, scroll, events) = ui.input(|i| {
            (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.latest_pos(), i.smooth_scroll_delta().y, i.events.clone())
        });
        let over_menu = |q: egui::Pos2| menu.is_some_and(|r| r.contains(q));
        if self.m.tool.is_some() && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if pressed {
            if let Some(q) = latest.filter(|q| stage.contains(*q) && !over_menu(*q)) {
                self.focused = true;
                if self.menu_open {
                    self.menu_open = false;
                }
                let now = Instant::now();
                let clicks = match self.last_press {
                    Some((t, pp)) if now.duration_since(t) < Duration::from_millis(400) && pp.distance(q) < 5.0 => 2,
                    _ => 1,
                };
                self.last_press = Some((now, q));
                let f = self.m.mouse_down(p(q.x, q.y), clicks);
                self.refresh(f);
            } else if latest.is_some_and(|q| !over_menu(q)) {
                self.focused = false;
            }
        }
        if latest != self.last_pointer {
            self.last_pointer = latest;
            let q = latest.unwrap_or(pos2(-1.0, -1.0));
            let inside = latest.is_some_and(|q| stage.contains(q) && !over_menu(q));
            let m = &self.m;
            if inside || m.mouse.is_some() || m.pan.is_some() || m.drag.is_some() || m.pending.is_some() {
                let f = self.m.mouse_move(p(q.x, q.y), inside);
                self.refresh(f);
            }
        }
        if released {
            let m = &self.m;
            if m.pan.is_some() || m.drag.is_some() || m.pending.as_ref().is_some_and(|p| p.dragging) || m.busy {
                let q = latest.unwrap_or(pos2(-1.0, -1.0));
                let f = self.m.mouse_up(p(q.x, q.y));
                self.refresh(f);
            }
        }
        if scroll != 0.0 && resp.hovered() {
            let f = self.m.wheel(scroll);
            self.refresh(f);
        }
        if self.focused {
            for e in events {
                let f = match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        if self.m.text_edit.is_some() {
                            match key {
                                Key::Enter => self.m.commit_text(true),
                                Key::Escape => self.m.commit_text(false),
                                Key::Backspace => {
                                    self.m.text_edit.as_mut().unwrap().text.pop();
                                    LIVE
                                }
                                _ => 0,
                            }
                        } else {
                            let ctrl = modifiers.ctrl || modifiers.command;
                            match key {
                                Key::Delete | Key::Backspace => self.m.delete_selected(),
                                Key::Escape => self.m.escape(),
                                // 上下順序：Ctrl+] 上移一層、Ctrl+[ 下移一層、加 Shift 移到最上／最下
                                Key::CloseBracket if ctrl => self.m.zop(if modifiers.shift { ZOp::Front } else { ZOp::Forward }),
                                Key::OpenBracket if ctrl => self.m.zop(if modifiers.shift { ZOp::Back } else { ZOp::Backward }),
                                _ => 0,
                            }
                        }
                    }
                    egui::Event::Text(t) if self.m.text_edit.is_some() => {
                        self.m.text_edit.as_mut().unwrap().text.push_str(&t);
                        LIVE
                    }
                    _ => 0,
                };
                self.refresh(f);
            }
        }
    }

    fn show_rail(&mut self, ui: &mut Ui, rail: Rect) {
        let painter = ui.painter_at(rail);
        painter.rect_filled(rail, 0.0, panel());
        painter.vline(rail.max.x - 0.5, rail.y_range(), (1.0, border()));
        let tool = self.m.tool;
        let can_delete = self.m.selected.is_some();
        let can_clear = !self.m.drawings.is_empty();
        let mut y = rail.min.y + 4.0;
        let x = rail.min.x + 3.0;
        let mut act: Option<u8> = None; // 0..=8 tool, 9 delete, 10 clear
        let mut btn = |ui: &mut Ui, y: &mut f32, id: u8, title: &str, icon: Icon, on: bool, enabled: bool| {
            let r = Rect::from_min_size(pos2(x, *y), vec2(28.0, 28.0));
            *y += 30.0;
            let resp = ui.interact(r, ui.scope_id().with(("rail", id)), if enabled { Sense::click() } else { Sense::hover() }).on_hover_text(title);
            let p = ui.painter_at(r);
            if on {
                p.rect(r, 2.0, rgb(tk().toolbar.button_on_bg), (1.0, accent()), egui::StrokeKind::Inside);
            } else if enabled && resp.hovered() {
                p.rect_filled(r, 2.0, rgb(tk().toolbar.button_hover));
            }
            let c = if !enabled { rgba(tk().toolbar.icon_disabled.rgb, tk().toolbar.icon_disabled.a) } else if on { rgb(tk().toolbar.icon_on) } else { rgb(tk().toolbar.icon_off) };
            paint_icon(&p, icon, Rect::from_center_size(r.center(), vec2(18.0, 18.0)), c);
            if enabled && resp.clicked() {
                act = Some(id);
            }
        };
        btn(ui, &mut y, 0, "游標", Icon::Cursor, tool.is_none(), true);
        painter.hline((x + 3.0)..=(x + 25.0), y + 1.0, (1.0, rgb(tk().toolbar.separator)));
        y += 4.0;
        for (k, t) in Tool::ALL.iter().enumerate() {
            btn(ui, &mut y, 1 + k as u8, t.label(), Icon::Tool(*t), tool == Some(*t), true);
        }
        painter.hline((x + 3.0)..=(x + 25.0), y + 1.0, (1.0, rgb(tk().toolbar.separator)));
        y += 4.0;
        btn(ui, &mut y, 9, "刪除選取", Icon::Delete, false, can_delete);
        btn(ui, &mut y, 10, "全部清除", Icon::Clear, false, can_clear);
        if let Some(a) = act {
            let f = match a {
                0 => self.m.set_tool(None),
                9 => self.m.delete_selected(),
                10 => self.m.clear_drawings(),
                k => self.m.set_tool(Some(Tool::ALL[(k - 1) as usize])),
            };
            self.refresh(f);
        }
    }

    /// 圖層面板：選取畫線的上下順序＋圖層清單（最上層在最上面；顯示／鎖定／上移／下移；點名稱設為作用中圖層）
    fn show_layers(&mut self, ctx: &Context, at: egui::Pos2) -> Rect {
        let m = &self.m;
        let sel_layer = m.selected.and_then(|k| m.drawings.get(k)).map(|d| d.layer);
        let (layers, counts, active) = (m.layers.clone(), m.layer_counts(), m.active_layer);
        let has_sel = sel_layer.is_some();
        let mut f: u8 = 0;
        let r = egui::Area::new(egui::Id::unique("layers-panel")).fixed_pos(at).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(panel()).stroke((1.0, border())).show(ui, |ui| {
                ui.set_width(340.0);
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(tk().font.sm));
                ui.label(egui::RichText::new(if has_sel { "選取的畫線（在自己的圖層內調整）" } else { "選取一條畫線後可調整上下順序" }).color(dim()));
                ui.horizontal_wrapped(|ui| {
                    for op in ZOp::ALL {
                        if ui.add_enabled(has_sel, egui::Button::new(format!("{}（{}）", op.label(), op.keys())).small()).clicked() {
                            f |= self.m.zop(op);
                        }
                    }
                    let can_move = has_sel && sel_layer != Some(active);
                    if ui.add_enabled(can_move, egui::Button::new("移到作用中圖層").small()).clicked() {
                        f |= self.m.move_selected_to_layer(active);
                    }
                });
                ui.separator();
                let n = layers.len();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("圖層（{n}，上面的蓋住下面的）")).color(dim()));
                    if ui.add_enabled(n < crate::layers::MAX_LAYERS, egui::Button::new("+ 新圖層").small()).clicked() {
                        f |= self.m.layer_add();
                    }
                });
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for (pos, l) in layers.iter().enumerate().rev() {
                        let id = l.id;
                        let is_active = id == active;
                        ui.horizontal(|ui| {
                            if ui.add(egui::Button::new(if l.visible { "顯示" } else { "隱藏" }).small().selected(!l.visible)).clicked() {
                                f |= self.m.layer_toggle_visible(id);
                            }
                            if ui.add(egui::Button::new(if l.locked { "鎖定" } else { "未鎖" }).small().selected(l.locked)).clicked() {
                                f |= self.m.layer_toggle_lock(id);
                            }
                            let name = format!(
                                "{}{}（{}）{}",
                                if is_active { "▶ " } else { "" },
                                l.name,
                                counts.get(pos).copied().unwrap_or(0),
                                if sel_layer == Some(id) { " ●" } else { "" }
                            );
                            let txt = egui::RichText::new(name).color(if l.visible { text() } else { dim() });
                            if fixed_w(ui, 170.0, |ui| ui.add(egui::Button::new(txt).small().frame(false).selected(is_active))).clicked() {
                                f |= self.m.set_active_layer(id);
                            }
                            if ui.add_enabled(pos + 1 < n, egui::Button::new("▲").small()).clicked() {
                                f |= self.m.layer_move(id, true);
                            }
                            if ui.add_enabled(pos > 0, egui::Button::new("▼").small()).clicked() {
                                f |= self.m.layer_move(id, false);
                            }
                        });
                    }
                });
            });
        });
        self.refresh(f);
        r.response.rect
    }

    fn show_menu(&mut self, ctx: &Context, at: egui::Pos2) -> Option<Rect> {
        let cfg = self.m.cfg.clone();
        let mut change: Option<Box<dyn FnOnce(&mut IndCfg)>> = None;
        let r = egui::Area::new(egui::Id::unique("ind-menu")).fixed_pos(at).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(panel()).stroke((1.0, border())).show(ui, |ui| {
                ui.set_width(410.0);
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(tk().font.sm));
                egui::ScrollArea::vertical().max_height(500.0).show(ui, |ui| {
                    ui.label(egui::RichText::new(format!("主圖疊加（{}/{MAX_OVERLAYS}）", cfg.overlays.len())).color(dim()));
                    for (k, o) in cfg.overlays.iter().enumerate() {
                        ui.horizontal(|ui| {
                            fixed_w(ui, 110.0, |ui| ui.label(egui::RichText::new(o.label()).color(rgb(ov_color(k)))));
                            if ui.small_button("−").clicked() {
                                change = Some(Box::new(move |c| step_ov(&mut c.overlays[k], -1.0)));
                            }
                            if ui.small_button("+").clicked() {
                                change = Some(Box::new(move |c| step_ov(&mut c.overlays[k], 1.0)));
                            }
                            if ui.small_button("×").clicked() {
                                change = Some(Box::new(move |c| {
                                    c.overlays.remove(k);
                                }));
                            }
                        });
                    }
                    let full = cfg.overlays.len() >= MAX_OVERLAYS;
                    ui.horizontal(|ui| {
                        for (kind, label) in [("ma", "+ MA"), ("ema", "+ EMA"), ("bb", "+ 布林"), ("sar", "+ SAR")] {
                            if ui.add_enabled(!full, egui::Button::new(label).small()).clicked() {
                                change = Some(Box::new(move |c| {
                                    if c.overlays.len() < MAX_OVERLAYS {
                                        c.overlays.push(OvSpec::defaults(kind));
                                    }
                                }));
                            }
                        }
                    });
                    ui.separator();
                    let n_on = cfg.subs.iter().filter(|s| s.on).count();
                    ui.label(egui::RichText::new(format!("副圖（{n_on}/{}，最多 {MAX_SUBS}）", cfg.subs.len())).color(dim()));
                    for (k, s) in cfg.subs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let mut on = s.on;
                            if fixed_w(ui, 130.0, |ui| ui.checkbox(&mut on, s.kind.label())).changed() {
                                change = Some(Box::new(move |c| {
                                    let on_now = c.subs.iter().filter(|s| s.on).count();
                                    if c.subs[k].on || on_now < MAX_SUBS {
                                        c.subs[k].on = !c.subs[k].on;
                                    }
                                }));
                            }
                            for (pi, (pn, _)) in s.kind.params().iter().enumerate() {
                                ui.label(egui::RichText::new(format!("{pn} {}", s.p[pi])).color(dim()));
                                if ui.small_button("−").clicked() {
                                    change = Some(Box::new(move |c| c.subs[k].p[pi] = (c.subs[k].p[pi] - 1.0).max(1.0)));
                                }
                                if ui.small_button("+").clicked() {
                                    change = Some(Box::new(move |c| c.subs[k].p[pi] = (c.subs[k].p[pi] + 1.0).min(250.0)));
                                }
                            }
                            if s.kind == SubKind::Vol {
                                let mut vm = cfg.volma.0;
                                if ui.checkbox(&mut vm, format!("均量 MA{}/{}", cfg.volma.1[0], cfg.volma.1[1])).changed() {
                                    change = Some(Box::new(|c| c.volma.0 = !c.volma.0));
                                }
                            }
                        });
                    }
                    if ui.small_button("恢復預設").clicked() {
                        change = Some(Box::new(|c| *c = IndCfg::default()));
                    }
                });
            });
        });
        if let Some(f) = change {
            self.change_cfg(f);
        }
        Some(r.response.rect)
    }
}

/// 固定寬度、靠左（`add_sized` 會置中，和 gpui 版的靠左欄位對不齊）
fn fixed_w(ui: &mut Ui, w: f32, add: impl FnOnce(&mut Ui) -> egui::Response) -> egui::Response {
    ui.allocate_ui_with_layout(vec2(w, 16.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_width(w);
        add(ui)
    })
    .inner
}

fn small_btn(ui: &mut Ui, label: &str, on: bool) -> egui::Response {
    let b = egui::Button::new(egui::RichText::new(label).size(tk().font.md).color(if on { text() } else { dim() }))
        .fill(if on { selected() } else { Color32::TRANSPARENT })
        .stroke((1.0, if on { accent() } else { border() }))
        .corner_radius(tk().radius.xs)
        .min_size(vec2(0.0, 18.0));
    ui.add(b)
}

fn step_ov(o: &mut OvSpec, d: f64) {
    match o {
        OvSpec::Ma { period } | OvSpec::Ema { period } => *period = ((*period as f64 + d).clamp(1.0, 500.0)) as usize,
        OvSpec::Bb { n, .. } => *n = ((*n as f64 + d).clamp(2.0, 500.0)) as usize,
        OvSpec::Sar { step, max } => {
            *step = ((*step + d * 0.01).clamp(0.01, *max) * 100.0).round() / 100.0;
        }
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Cursor,
    Tool(Tool),
    Delete,
    Clear,
}

/// 圖示用線段畫（與 gpui 版相同座標，24 格）
fn paint_icon(painter: &egui::Painter, icon: Icon, b: Rect, color: Color32) {
    let s = b.width() / 24.0;
    let q = |x: f32, y: f32| pos2(b.min.x + x * s, b.min.y + y * s);
    let st = egui::Stroke::new(tk().stroke.series, color);
    let poly = |pts: &[(f32, f32)]| {
        painter.add(Shape::line(pts.iter().map(|&(x, y)| q(x, y)).collect(), st));
    };
    let dot = |x: f32, y: f32| {
        painter.circle_filled(q(x, y), 1.6 * s, color);
    };
    match icon {
        Icon::Cursor => poly(&[(6.0, 4.0), (18.0, 12.5), (12.5, 14.0), (9.5, 20.0), (6.0, 4.0)]),
        Icon::Tool(Tool::Trend) => {
            poly(&[(4.0, 18.0), (20.0, 6.0)]);
            dot(4.0, 18.0);
            dot(20.0, 6.0);
        }
        Icon::Tool(Tool::Ray) => {
            poly(&[(5.0, 18.0), (19.0, 6.0), (22.0, 4.5)]);
            dot(5.0, 18.0);
        }
        Icon::Tool(Tool::Hline) => {
            poly(&[(3.0, 12.0), (21.0, 12.0)]);
            dot(12.0, 12.0);
        }
        Icon::Tool(Tool::Vline) => {
            poly(&[(12.0, 3.0), (12.0, 21.0)]);
            dot(12.0, 12.0);
        }
        Icon::Tool(Tool::Channel) => {
            poly(&[(4.0, 16.0), (16.0, 4.0)]);
            poly(&[(8.0, 20.0), (20.0, 8.0)]);
            dot(4.0, 16.0);
            dot(16.0, 4.0);
        }
        Icon::Tool(Tool::Fib) => {
            for y in [5.0, 10.0, 14.0, 19.0] {
                poly(&[(4.0, y), (20.0, y)]);
            }
            poly(&[(4.0, 5.0), (4.0, 19.0)]);
        }
        Icon::Tool(Tool::Rect) => poly(&[(4.0, 6.0), (20.0, 6.0), (20.0, 18.0), (4.0, 18.0), (4.0, 6.0)]),
        Icon::Tool(Tool::Text) => {
            poly(&[(5.0, 6.0), (19.0, 6.0)]);
            poly(&[(12.0, 6.0), (12.0, 18.0)]);
            poly(&[(8.0, 18.0), (16.0, 18.0)]);
        }
        Icon::Delete => {
            poly(&[(5.0, 7.0), (19.0, 7.0)]);
            poly(&[(9.0, 7.0), (9.0, 5.0), (15.0, 5.0), (15.0, 7.0)]);
            poly(&[(8.0, 7.0), (9.0, 19.0), (15.0, 19.0), (16.0, 7.0)]);
        }
        Icon::Clear => {
            poly(&[(5.0, 6.0), (19.0, 6.0)]);
            poly(&[(6.2, 6.0), (7.2, 20.0), (16.8, 20.0), (17.8, 6.0)]);
            for x in [9.0, 12.0, 15.0] {
                poly(&[(x, 10.0), (x, 16.0)]);
            }
        }
    }
}
