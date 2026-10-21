//! 報價表：自己做的虛擬化（ScrollArea::show_viewport），只為可見列產生圖元；
//! 可見列的圖元三角化成一個 Mesh 快取，內容／捲動位置／選取／hover 都沒變時下一幀直接沿用。
//! 行情只改有變的列（gen +1）；字串只在重建可見列時格式化（5 萬列也不預先建字串）。

use std::collections::HashMap;
use std::sync::Arc;

use egui::epaint::Mesh;
use egui::{pos2, vec2, Color32, Rect, ScrollArea, Sense, Shape, Ui};
use xq_feed::Quote;

use crate::gfx::{self, Align, Sink};
use crate::theme::*;

pub const ROW_H: f32 = 22.0;
const COLS: [(&str, f32); 6] = [("代號", 58.0), ("名稱", 92.0), ("成交", 76.0), ("漲跌", 70.0), ("幅度", 64.0), ("總量", 92.0)];
pub const STRESS_ROWS: usize = 50_000;
const HEAD_H: f32 = 26.0;
const COLHEAD_H: f32 = 22.0;

pub struct Row {
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub change: f64,
    pub volume: i64,
    pub tick: i8,
}

#[derive(Clone, Copy, PartialEq)]
struct Key {
    clip: Rect,
    origin_y: f32,
    i0: usize,
    i1: usize,
    gen: u64,
    selected: usize,
    hover: Option<usize>,
    font: ([usize; 2], u32),
}

pub struct QuoteTable {
    rows: Vec<Row>,
    index: HashMap<String, usize>,
    real_len: usize,
    pub selected: usize,
    pub stress: bool,
    gen: u64,
    cache: Option<(Key, Arc<Mesh>)>,
    scroll_to: Option<f32>,
    /// 上一幀的可見範圍（內容座標 y0, 高度）
    view: (f32, f32),
    /// 壓測量測：可見列重建次數／累積耗時（µs）／建立的格數（列×6 欄）
    pub paints: u64,
    pub paint_us: u64,
    pub cells: u64,
}

impl QuoteTable {
    pub fn new(watch: &[(&str, &str)]) -> Self {
        let rows: Vec<Row> = watch
            .iter()
            .map(|(s, n)| Row { symbol: (*s).into(), name: (*n).into(), price: 0.0, change: 0.0, volume: 0, tick: 0 })
            .collect();
        let index = rows.iter().enumerate().map(|(i, r)| (r.symbol.clone(), i)).collect();
        QuoteTable {
            real_len: rows.len(),
            rows,
            index,
            selected: 0,
            stress: false,
            gen: 0,
            cache: None,
            scroll_to: None,
            view: (0.0, 400.0),
            paints: 0,
            paint_us: 0,
            cells: 0,
        }
    }

    pub fn symbols(&self) -> Vec<String> {
        self.rows[..self.real_len].iter().map(|r| r.symbol.clone()).collect()
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn row(&self, symbol: &str) -> Option<&Row> {
        self.index.get(symbol).map(|&i| &self.rows[i])
    }

    /// 只碰有變的列；回傳變動列數。
    pub fn apply(&mut self, order: &[String], quotes: &HashMap<String, Quote>) -> usize {
        let mut changed = 0;
        for sym in order {
            let Some(q) = quotes.get(sym) else { continue };
            let Some(&i) = self.index.get(sym.as_str()) else { continue };
            let r = &mut self.rows[i];
            if r.price == q.price && r.change == q.change && r.volume == q.volume {
                continue;
            }
            r.tick = if r.price == 0.0 || q.price == r.price { r.tick } else if q.price > r.price { 1 } else { -1 };
            r.price = q.price;
            r.change = q.change;
            r.volume = q.volume;
            if !q.name.is_empty() && q.name != q.symbol && r.name != q.name {
                r.name = q.name.clone();
            }
            changed += 1;
        }
        if changed > 0 {
            self.gen += 1;
        }
        changed
    }

    pub fn toggle_stress(&mut self) {
        self.stress = !self.stress;
        self.rows.truncate(self.real_len);
        self.index.retain(|_, i| *i < self.real_len);
        if self.stress {
            self.rows.reserve_exact(STRESS_ROWS);
            for k in 0..STRESS_ROWS {
                let h = (k as u64).wrapping_mul(2654435761) % 100_000;
                let price = 10.0 + (h % 90_000) as f64 / 100.0;
                let change = ((h % 400) as f64 - 200.0) / 100.0;
                self.rows.push(Row { symbol: format!("Z{k:05}"), name: "壓力測試".into(), price, change, volume: (h * 7) as i64, tick: 0 });
            }
        } else {
            self.rows.shrink_to_fit();
            if self.selected >= self.real_len {
                self.selected = 0;
            }
        }
        self.gen += 1;
    }

    /// XQ_TICK_BENCH=1：開 5 萬合成列並把 Z 代號放進索引
    pub fn enable_stress_indexed(&mut self) {
        if !self.stress {
            self.toggle_stress();
        }
        self.index.reserve(self.rows.len() - self.real_len);
        for i in self.real_len..self.rows.len() {
            self.index.insert(self.rows[i].symbol.clone(), i);
        }
    }

    /// 壓測：在 5 萬合成列裡隨機挑 n 列跳價（同 wry／GPUI 版）
    pub fn synth_ticks(&mut self, n: usize, rng: &mut u64) {
        let base = self.real_len;
        let cnt = self.rows.len().saturating_sub(base);
        if !self.stress || cnt == 0 {
            return;
        }
        let mut next = || {
            *rng ^= *rng << 13;
            *rng ^= *rng >> 7;
            *rng ^= *rng << 17;
            *rng
        };
        for _ in 0..n {
            let i = base + (next() % cnt as u64) as usize;
            let u = (next() % 10_000) as f64 / 10_000.0 - 0.5;
            let r = &mut self.rows[i];
            let wobble = u * (r.price.abs() * 0.002).max(0.2);
            r.price += wobble;
            r.change += wobble;
            r.volume += (next() % 200) as i64;
            r.tick = if wobble >= 0.0 { 1 } else { -1 };
        }
        self.gen += 1;
    }

    /// 壓測：捲到第 frac 的位置（同 GPUI scroll_to_item(ix, Top)）
    pub fn scroll_frac(&mut self, frac: f64) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let ix = ((n - 1) as f64 * frac.clamp(0.0, 1.0)) as usize;
        self.scroll_to = Some(ix as f32 * ROW_H);
    }

    fn select(&mut self, ix: usize) -> Option<String> {
        if ix >= self.rows.len() {
            return None;
        }
        self.selected = ix;
        // Nearest：在可見範圍外才捲
        let (y0, h) = self.view;
        let top = ix as f32 * ROW_H;
        if top < y0 {
            self.scroll_to = Some(top);
        } else if top + ROW_H > y0 + h {
            self.scroll_to = Some(top + ROW_H - h);
        }
        Some(self.rows[ix].symbol.clone())
    }

    /// 畫在 rect 裡；點選（滑鼠按下就選取）回傳代號
    pub fn show(&mut self, ui: &mut Ui, rect: Rect) -> Option<String> {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, PANEL);
        // 標頭
        let hr = Rect::from_min_size(rect.min, vec2(rect.width(), HEAD_H));
        painter.rect_filled(hr, 0.0, HEADER);
        painter.hline(hr.x_range(), hr.max.y - 0.5, (1.0, BORDER));
        let f12 = egui::FontId::proportional(12.0);
        let tw = painter.text(pos2(hr.min.x + 8.0, hr.center().y), egui::Align2::LEFT_CENTER, "報價", f12.clone(), TEXT).width();
        painter.text(pos2(hr.min.x + 16.0 + tw, hr.center().y), egui::Align2::LEFT_CENTER, format!("{} 檔", fmt_int(self.rows.len() as i64)), f12.clone(), DIM);
        let br = Rect::from_min_size(pos2(hr.max.x - 80.0, hr.min.y + 4.0), vec2(72.0, HEAD_H - 8.0));
        let bresp = ui.interact(br, ui.scope_id().with("stress-btn"), Sense::click());
        let on = self.stress;
        painter.rect(br, 2.0, if bresp.hovered() { SELECTED } else { Color32::TRANSPARENT }, (1.0, if on { ACCENT } else { BORDER }), egui::StrokeKind::Inside);
        painter.text(br.center(), egui::Align2::CENTER_CENTER, "壓力 5 萬", f12.clone(), if on { ACCENT } else { DIM });
        if bresp.clicked() {
            self.toggle_stress();
        }
        // 欄位標頭
        let cr = Rect::from_min_size(pos2(rect.min.x, hr.max.y), vec2(rect.width(), COLHEAD_H));
        painter.hline(cr.x_range(), cr.max.y - 0.5, (1.0, BORDER));
        let mut x = cr.min.x;
        for (i, (label, w)) in COLS.iter().enumerate() {
            let (px, al) = if i >= 2 { (x + w - 4.0, egui::Align2::RIGHT_CENTER) } else { (x + 4.0, egui::Align2::LEFT_CENTER) };
            painter.text(pos2(px, cr.center().y), al, *label, f12.clone(), DIM);
            x += w;
        }
        // 列（虛擬化）
        let body = Rect::from_min_max(pos2(rect.min.x, cr.max.y), rect.max);
        let n = self.rows.len();
        let mut picked = None;
        // 捲軸常駐＋關掉捲動動畫：egui 的捲軸淡入淡出／平滑捲動每一步都會 request_repaint()（delay=0 → 兩幀），
        // 程式捲動時會多畫一倍的幀；看盤表格不需要這些動畫
        let mut sa = ScrollArea::vertical()
            .id_salt("quotes")
            .auto_shrink([false, false])
            .animated(false)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
        if let Some(off) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset(off);
        }
        let ctx = ui.ctx().clone();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
        // 關掉慣性／橡皮筋：預設的 physics.step 每幀都會 request_repaint()（實測跳價中約 60% 的幀是它叫的），
        // 看盤表格只要滾輪＋捲軸
        child.spacing_mut().scroll.kinetic =
            egui::style::KineticScrollStyle { decay_time: 0.0, stop_distance: 0.5, rubber_band: false };
        sa.show_viewport(&mut child, |ui, vp| {
            let origin = ui.max_rect().min;
            let w = ui.available_width();
            ui.allocate_rect(Rect::from_min_size(origin, vec2(w, n as f32 * ROW_H)), Sense::hover());
            self.view = (vp.min.y, vp.height());
            let clip = ui.clip_rect();
            let i0 = ((vp.min.y / ROW_H).floor().max(0.0) as usize).min(n);
            let i1 = ((vp.max.y / ROW_H).ceil().max(0.0) as usize).min(n);
            let resp = ui.interact(clip, ui.scope_id().with("rows"), Sense::click());
            let row_at = |y: f32| ((y - origin.y) / ROW_H).floor();
            let hover = resp.hover_pos().map(|q| row_at(q.y)).filter(|r| *r >= 0.0 && (*r as usize) < n).map(|r| r as usize);
            if resp.contains_pointer() && ui.input(|i| i.pointer.primary_pressed()) {
                if let Some(ix) = hover {
                    picked = self.select(ix);
                    self.gen += 1;
                }
            }
            let key = Key { clip, origin_y: origin.y, i0, i1, gen: self.gen, selected: self.selected, hover, font: gfx::font_key(&ctx) };
            if gfx::no_view_cache() || !matches!(&self.cache, Some((k, _)) if *k == key) {
                let t0 = std::time::Instant::now();
                let mesh = self.build_rows(&ctx, origin.x, origin.y, w, i0, i1, hover);
                if i1 > i0 {
                    self.paints += 1;
                }
                self.cells += ((i1 - i0) * COLS.len()) as u64;
                self.paint_us += t0.elapsed().as_micros() as u64;
                self.cache = Some((key, Arc::new(mesh)));
            }
            ui.painter().add(Shape::Mesh(self.cache.as_ref().unwrap().1.clone()));
        });
        picked
    }

    #[allow(clippy::too_many_arguments)]
    fn build_rows(&self, ctx: &egui::Context, x0: f32, oy: f32, w: f32, i0: usize, i1: usize, hover: Option<usize>) -> Mesh {
        let mut s = Sink::default();
        for ix in i0..i1 {
            let r = &self.rows[ix];
            let y = oy + ix as f32 * ROW_H;
            let ym = y + ROW_H / 2.0;
            if ix == self.selected {
                s.rect(x0, y, w, ROW_H, SELECTED);
            } else if hover == Some(ix) {
                s.rect(x0, y, w, ROW_H, HEADER);
            }
            let color = dir_color(r.change);
            let has = r.price > 0.0;
            let mut x = x0;
            let cells: [(String, Color32); 6] = if has {
                [
                    (r.symbol.clone(), TEXT),
                    (r.name.clone(), TEXT),
                    (format!("{:.2}", r.price), color),
                    (fmt_change(r.change), color),
                    (format!("{:+.2}%", pct(r.price, r.change)), color),
                    (fmt_int(r.volume), VOL),
                ]
            } else {
                [
                    (r.symbol.clone(), TEXT),
                    (r.name.clone(), TEXT),
                    ("--".into(), color),
                    ("--".into(), color),
                    ("--".into(), color),
                    ("--".into(), VOL),
                ]
            };
            for (i, (txt, c)) in cells.into_iter().enumerate() {
                let cw = COLS[i].1;
                if i == 2 {
                    match r.tick {
                        1 => s.rect(x, y, cw, ROW_H, Color32::from_rgba_unmultiplied(0xff, 0x4d, 0x4f, 0x22)),
                        -1 => s.rect(x, y, cw, ROW_H, Color32::from_rgba_unmultiplied(0x22, 0xc5, 0x5e, 0x22)),
                        _ => {}
                    }
                }
                if i >= 2 {
                    s.text_at(txt, x + cw - 4.0, ym, 12.0, c, Align::Right);
                } else {
                    s.text_ui(txt, x + 4.0, ym, 12.0, c, Align::Left);
                }
                x += cw;
            }
        }
        s.finish(ctx)
    }
}
