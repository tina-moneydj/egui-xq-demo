//! 成交明細面板（tick tape）—— 與 gpui-xq-demo main 的 `src/tick_tape.rs` 同一套行為，egui 立即模式版。
//!
//! 開法：`XQ_TAPE=1 ./target/release/egui-xq-demo`（右下多一欄 276px；預設關閉，壓測版面不變）。
//! 純邏輯（推導成交、環狀緩衝、格式化）直接用 gpui 倉庫的 `crates/xq-tape`（同一個 rev），這裡只做畫面。
//!
//! 對照 GPUI 版的五個【模式】：
//! - A 背景→UI：`TapeFeed` 在行情執行緒 O(1) 累積進固定容量 Ring，UI 每幀最多換一次（雙緩衝 swap）
//! - B 狀態：`TickTape` 是一般 struct；有新成交才 `gen += 1`（egui 沒有 notify，靠 gen 判斷快取失效）
//! - C 虛擬化：只為可見列格式化、三角化成一個 Mesh 快取；沒有新成交、沒捲動就沿用同一個 Arc<Mesh>
//! - D 快捷鍵＋焦點：點面板取得焦點後 `b`／`Home`／`]`／`[` 才生效（等同 GPUI 的 key_context）
//! - E 存檔不在 UI 執行緒：讀檔、寫檔都在背景執行緒；寫檔前防抖 450 ms
//!
//! 捲動：和 GPUI main 的 uniform_list 一樣「記住像素位移」——捲離頂端時新成交進來，內容會往下推
//! （tick-tape-scroll-anchor 分支的「錨定」行為沒有合併進 main，這裡也不做）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use crate::plat::Instant;

use egui::epaint::{Mesh, Shape};
use egui::{pos2, vec2, Align2, Color32, FontId, Key, Rect, Sense, Ui};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use xq_feed::Quote;
use xq_tape::{format_row, Ring, TapeCore, Trade};

use crate::gfx::{self, Align, Sink};
use crate::theme::*;

/// 畫面最多保留幾筆（與 GPUI 版相同）
pub const TAPE_CAP: usize = 5000;
/// 背景佇列容量：UI 一幀內最多收幾筆；超過擠掉最舊的並計數
pub const PENDING_CAP: usize = 4096;
/// 面板寬（與 GPUI 版 `w(px(276.))` 相同）
pub const PANEL_W: f32 = 276.0;
const ROW_H: f32 = 20.0;
const HEAD_H: f32 = 26.0;
const COLS_H: f32 = 20.0;
const FOOT_H: f32 = 18.0;
const COLS: [(&str, f32); 4] = [("時間", 96.0), ("成交", 72.0), ("單量", 64.0), ("", 28.0)];
const BIG_FG: Color32 = rgb(0xf0c674);

// ───────────────── 【模式 A】背景執行緒 → UI ─────────────────

pub struct TapeShared {
    core: TapeCore,
    pending: Ring<Trade>,
}

#[derive(Clone)]
pub struct TapeFeed(Arc<Mutex<TapeShared>>);

impl TapeFeed {
    pub fn new(symbol: &str) -> Self {
        TapeFeed(Arc::new(Mutex::new(TapeShared { core: TapeCore::new(symbol), pending: Ring::with_capacity(PENDING_CAP) })))
    }

    /// **在行情執行緒上**被 `bridge::merge` 呼叫（合併前、每包一次）：O(1)／筆、不配置、不碰 UI。
    pub fn on_quotes(&self, quotes: &[Quote]) {
        let now = crate::plat::unix_us() / 1000;
        let mut s = self.0.lock();
        let TapeShared { core, pending } = &mut *s;
        for q in quotes {
            if let Some(t) = core.on_quote(q, now) {
                pending.push(t);
            }
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.0.lock().pending.is_empty()
    }

    fn set_symbol(&self, symbol: &str) {
        let mut s = self.0.lock();
        s.core.set_symbol(symbol);
        s.pending.clear();
    }
}

// ───────────────── 【模式 E】設定與存檔 ─────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TapeSettings {
    pub big_only: bool,
    pub big_qty: i64,
}

impl Default for TapeSettings {
    fn default() -> Self {
        TapeSettings { big_only: false, big_qty: 50 }
    }
}

impl TapeSettings {
    /// 門檻階梯：1 → 5 → 10 → 20 → 50 → 100 → 200 → 500 → 1000
    pub fn step_big_qty(&self, up: bool) -> i64 {
        const STEPS: [i64; 9] = [1, 5, 10, 20, 50, 100, 200, 500, 1000];
        let i = STEPS.iter().position(|&s| s >= self.big_qty).unwrap_or(STEPS.len() - 1);
        let j = if up { (i + 1).min(STEPS.len() - 1) } else { i.saturating_sub(1) };
        STEPS[j]
    }
}

fn settings_path() -> Option<PathBuf> {
    if let Some(p) = crate::plat::var_os("XQ_TAPE_STATE") {
        return Some(PathBuf::from(p));
    }
    let base = crate::plat::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::plat::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("egui-xq-demo").join("tick_tape.json"))
}

fn load_settings(path: Option<PathBuf>) -> TapeSettings {
    path.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_settings(path: PathBuf, json: String) {
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = result {
        eprintln!("[tick_tape] 設定存檔失敗 {}: {e}", path.display());
    }
}

// ───────────────── 【模式 B】狀態 ─────────────────

#[derive(Clone, Copy, PartialEq)]
struct RowsKey {
    body: Rect,
    gen: u64,
    offset_bits: u32,
    font: ([usize; 2], u32),
}

pub struct TickTape {
    feed: TapeFeed,
    all: Ring<Trade>,
    big: Ring<Trade>,
    spare: Ring<Trade>,
    symbol: String,
    settings: TapeSettings,
    focused: bool,
    /// 捲動位移（像素，0 = 最新在最上面）
    offset: f32,
    utc_offset_sec: i64,
    persist: bool,
    /// 背景讀檔的結果（讀完放這裡，下一幀套用）
    loaded: Option<Arc<Mutex<Option<TapeSettings>>>>,
    save_at: Option<Instant>,
    /// 內容版本：新成交、換代號、改設定都 +1（可見列 Mesh 快取的鍵）
    gen: u64,
    cache: Option<(RowsKey, Arc<Mesh>)>,
    pub applied: u64,
    pub dropped: u64,
}

impl TickTape {
    pub fn new(feed: TapeFeed, symbol: &str, ctx: &egui::Context) -> Self {
        let persist = !["XQ_BENCH", "XQ_TICK_BENCH", "XQ_CHART_STRESS", "XQ_GROUP_STRESS"]
            .iter()
            .any(|k| crate::plat::var_os(k).is_some_and(|v| v != "0"));
        let loaded = persist.then(|| {
            let slot = Arc::new(Mutex::new(None));
            let (s2, ctx) = (slot.clone(), ctx.clone());
            let path = settings_path();
            std::thread::spawn(move || {
                *s2.lock() = Some(load_settings(path));
                ctx.request_repaint();
            });
            slot
        });
        TickTape {
            feed,
            all: Ring::with_capacity(TAPE_CAP),
            big: Ring::with_capacity(TAPE_CAP),
            spare: Ring::with_capacity(PENDING_CAP),
            symbol: symbol.to_string(),
            settings: TapeSettings::default(),
            focused: false,
            offset: 0.0,
            utc_offset_sec: 8 * 3600,
            persist,
            loaded,
            save_at: None,
            gen: 0,
            cache: None,
            applied: 0,
            dropped: 0,
        }
    }

    /// 由 `Terminal::apply_feed` 呼叫（≤ 每幀一次，和報價表同一個節奏）。回傳新增幾筆。
    pub fn apply(&mut self) -> usize {
        {
            let mut s = self.feed.0.lock();
            if s.pending.is_empty() {
                return 0;
            }
            std::mem::swap(&mut s.pending, &mut self.spare);
        }
        let n = self.spare.len();
        self.dropped += std::mem::take(&mut self.spare.evicted);
        let big_qty = self.settings.big_qty;
        let (all, big) = (&mut self.all, &mut self.big);
        self.spare.drain_oldest_first(|t| {
            all.push(t);
            if t.qty >= big_qty {
                big.push(t);
            }
        });
        self.applied += n as u64;
        self.gen += 1;
        n
    }

    pub fn set_symbol(&mut self, symbol: &str) {
        if self.symbol == symbol {
            return;
        }
        self.feed.set_symbol(symbol);
        self.symbol = symbol.to_string();
        self.all.clear();
        self.big.clear();
        self.gen += 1;
    }

    fn rows(&self) -> &Ring<Trade> {
        if self.settings.big_only { &self.big } else { &self.all }
    }

    fn set_settings(&mut self, s: TapeSettings, save: bool) {
        if s == self.settings {
            return;
        }
        let rebuild = s.big_qty != self.settings.big_qty;
        self.settings = s;
        if rebuild {
            self.big.clear();
            for i in (0..self.all.len()).rev() {
                if let Some(t) = self.all.newest(i).filter(|t| t.qty >= self.settings.big_qty) {
                    self.big.push(t);
                }
            }
        }
        if save && self.persist {
            self.save_at = Some(Instant::now() + Duration::from_millis(450));
        }
        self.gen += 1;
    }

    fn toggle_big_only(&mut self) {
        let s = TapeSettings { big_only: !self.settings.big_only, ..self.settings.clone() };
        self.set_settings(s, true);
        self.offset = 0.0;
    }

    fn step(&mut self, up: bool) {
        let s = TapeSettings { big_qty: self.settings.step_big_qty(up), ..self.settings.clone() };
        self.set_settings(s, true);
    }

    /// 可見列範圍（純函式，好測）：位移 `offset`、可見高度 `h`、共 `n` 列
    pub fn visible_range(offset: f32, h: f32, n: usize) -> std::ops::Range<usize> {
        let i0 = ((offset / ROW_H).floor().max(0.0) as usize).min(n);
        let i1 = (((offset + h) / ROW_H).ceil().max(0.0) as usize).min(n);
        i0..i1
    }

    // ───────────────── 畫面 ─────────────────

    pub fn show(&mut self, ui: &mut Ui, r: Rect) {
        let ctx = ui.ctx().clone();
        let got = self.loaded.as_ref().and_then(|slot| slot.lock().take());
        if let Some(s) = got {
            self.loaded = None;
            self.set_settings(s, false);
        }
        if let Some(t) = self.save_at {
            let now = Instant::now();
            if now >= t {
                self.save_at = None;
                if let (Some(path), Ok(json)) = (settings_path(), serde_json::to_string(&self.settings)) {
                    std::thread::spawn(move || save_settings(path, json));
                }
            } else {
                ctx.request_repaint_after(t - now);
            }
        }

        // 焦點與快捷鍵【模式 D】
        let resp = ui.interact(r, ui.scope_id().with("tick-tape"), Sense::click());
        let pressed = ui.input(|i| i.pointer.primary_pressed().then(|| i.pointer.latest_pos()).flatten());
        if let Some(q) = pressed {
            self.focused = r.contains(q);
        }
        if self.focused {
            let (b, home, up, down) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::B),
                    i.consume_key(egui::Modifiers::NONE, Key::Home),
                    i.consume_key(egui::Modifiers::NONE, Key::CloseBracket),
                    i.consume_key(egui::Modifiers::NONE, Key::OpenBracket),
                )
            });
            if b {
                self.toggle_big_only();
            }
            if home {
                self.offset = 0.0;
                self.gen += 1;
            }
            if up {
                self.step(true);
            }
            if down {
                self.step(false);
            }
        }

        let painter = ui.painter_at(r);
        painter.rect_filled(r, 0.0, PANEL);
        let f12 = FontId::proportional(12.0);
        // 標頭
        let hr = Rect::from_min_size(r.min, vec2(r.width(), HEAD_H));
        painter.rect_filled(hr, 0.0, HEADER);
        painter.hline(hr.x_range(), hr.max.y - 0.5, (1.0, BORDER));
        let count = self.rows().len();
        let mut x = hr.min.x + 8.0;
        for (s, c) in [("成交明細".to_string(), TEXT), (self.symbol.clone(), DIM), (format!("{} 筆", fmt_int(count as i64)), DIM)] {
            x += painter.text(pos2(x, hr.center().y), Align2::LEFT_CENTER, s, f12.clone(), c).width() + 8.0;
        }
        let label = format!("大單 ≥ {}", fmt_int(self.settings.big_qty));
        let lw = ctx.fonts_mut(|f| f.layout_no_wrap(label.clone(), f12.clone(), TEXT).size().x);
        let br = Rect::from_min_max(pos2(hr.max.x - 8.0 - lw - 16.0, hr.min.y + 4.0), pos2(hr.max.x - 8.0, hr.max.y - 4.0));
        let bresp = ui.interact(br, ui.scope_id().with("tape-big"), Sense::click());
        let on = self.settings.big_only;
        painter.rect(br, 2.0, if bresp.hovered() { SELECTED } else { Color32::TRANSPARENT }, (1.0, if on { ACCENT } else { BORDER }), egui::StrokeKind::Inside);
        painter.text(br.center(), Align2::CENTER_CENTER, label, f12.clone(), if on { ACCENT } else { DIM });
        if bresp.clicked() {
            self.focused = true;
            self.toggle_big_only();
        }
        // 欄名
        let cr = Rect::from_min_size(pos2(r.min.x, hr.max.y), vec2(r.width(), COLS_H));
        painter.hline(cr.x_range(), cr.max.y - 0.5, (1.0, BORDER));
        let mut cx = cr.min.x;
        for (i, (l, w)) in COLS.iter().enumerate() {
            if i == 0 {
                painter.text(pos2(cx + 4.0, cr.center().y), Align2::LEFT_CENTER, *l, f12.clone(), DIM);
            } else {
                painter.text(pos2(cx + w - 4.0, cr.center().y), Align2::RIGHT_CENTER, *l, f12.clone(), DIM);
            }
            cx += w;
        }
        // 列表（虛擬化）【模式 C】
        let body = Rect::from_min_max(pos2(r.min.x, cr.max.y), pos2(r.max.x, r.max.y - FOOT_H));
        let max_off = (count as f32 * ROW_H - body.height()).max(0.0);
        if resp.hovered() || resp.contains_pointer() {
            let dy = ui.input(|i| i.smooth_scroll_delta().y);
            if dy != 0.0 {
                self.offset -= dy;
            }
        }
        self.offset = self.offset.clamp(0.0, max_off);
        let key = RowsKey { body, gen: self.gen, offset_bits: self.offset.to_bits(), font: gfx::font_key(&ctx) };
        if gfx::no_view_cache() || !matches!(&self.cache, Some((k, _)) if *k == key) {
            let mesh = self.build_rows(&ctx, body);
            self.cache = Some((key, Arc::new(mesh)));
        }
        painter.with_clip_rect(body).add(Shape::Mesh(self.cache.as_ref().unwrap().1.clone()));
        // 頁尾
        let fr = Rect::from_min_max(pos2(r.min.x, body.max.y), r.max);
        painter.text(
            pos2(fr.min.x + 8.0, fr.center().y),
            Align2::LEFT_CENTER,
            if self.focused { "b 只看大單 · [ ] 門檻 · Home 回最新" } else { "點一下面板啟用快捷鍵" },
            FontId::proportional(10.0),
            DIM,
        );
        painter.rect_stroke(r.shrink(0.5), 0.0, (1.0, if self.focused { ACCENT } else { BORDER }), egui::StrokeKind::Inside);
    }

    fn build_rows(&self, ctx: &egui::Context, body: Rect) -> Mesh {
        let rows = self.rows();
        let mut s = Sink::default();
        let range = Self::visible_range(self.offset, body.height(), rows.len());
        for ix in range {
            let Some(t) = rows.newest(ix) else { continue };
            let row = format_row(&t, self.utc_offset_sec);
            let y = body.min.y + ix as f32 * ROW_H - self.offset;
            let ym = y + ROW_H / 2.0;
            let big = t.qty >= self.settings.big_qty;
            if big {
                s.rect(body.min.x, y, body.width(), ROW_H, Color32::from_rgba_unmultiplied(0xf0, 0xc6, 0x74, 0x18));
            }
            let color = match row.color {
                "up" => UP,
                "down" => DOWN,
                _ => FLAT,
            };
            let mut x = body.min.x;
            s.text_at(row.time, x + 4.0, ym, 12.0, DIM, Align::Left);
            x += COLS[0].1;
            s.text_at(row.price, x + COLS[1].1 - 4.0, ym, 12.0, color, Align::Right);
            x += COLS[1].1;
            s.text_at(row.qty, x + COLS[2].1 - 4.0, ym, 12.0, if big { BIG_FG } else { TEXT }, Align::Right);
            x += COLS[2].1;
            s.text_at(row.side, x + COLS[3].1 - 4.0, ym, 12.0, color, Align::Right);
        }
        s.finish(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_qty_steps_clamp_at_both_ends() {
        let s = |q| TapeSettings { big_only: false, big_qty: q };
        assert_eq!(s(50).step_big_qty(true), 100);
        assert_eq!(s(50).step_big_qty(false), 20);
        assert_eq!(s(1).step_big_qty(false), 1);
        assert_eq!(s(1000).step_big_qty(true), 1000);
        assert_eq!(s(30).step_big_qty(true), 100);
    }

    #[test]
    fn settings_tolerate_missing_fields_and_bad_files() {
        let s: TapeSettings = serde_json::from_str(r#"{"big_only":true}"#).expect("舊格式也要讀得進來");
        assert_eq!(s, TapeSettings { big_only: true, big_qty: 50 });
        let dir = std::env::temp_dir().join(format!("egui-xq-tape-test-{}", std::process::id()));
        let p = dir.join("tick_tape.json");
        save_settings(p.clone(), "{ 壞掉的 json".into());
        assert_eq!(load_settings(Some(p.clone())), TapeSettings::default());
        save_settings(p.clone(), serde_json::to_string(&TapeSettings { big_only: true, big_qty: 200 }).unwrap_or_default());
        assert_eq!(load_settings(Some(p)), TapeSettings { big_only: true, big_qty: 200 });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn feed_buffers_only_current_symbol_and_caps_memory() {
        let feed = TapeFeed::new("2330");
        let q = |sym: &str, v| Quote { symbol: sym.into(), name: String::new(), price: 1000.0, change: 0.0, volume: v };
        let quotes: Vec<Quote> = (0..(PENDING_CAP as i64 + 11)).map(|v| q("2330", v)).collect();
        feed.on_quotes(&quotes);
        feed.on_quotes(&[q("2317", 5)]);
        let s = feed.0.lock();
        assert_eq!(s.pending.len(), PENDING_CAP);
        assert_eq!(s.pending.evicted, 10, "UI 沒來取：超過容量的最舊幾筆被擠掉並計數");
    }

    #[test]
    fn apply_coalesces_a_whole_batch_per_frame_and_splits_big_trades() {
        let feed = TapeFeed::new("2330");
        let ctx = egui::Context::default();
        let mut tape = TickTape::new(feed.clone(), "2330", &ctx);
        tape.persist = false;
        let q = |v| Quote { symbol: "2330".into(), name: String::new(), price: 1000.0, change: 1.0, volume: v };
        // 基準 0 → 10（10 張）→ 70（60 張，大單）→ 71（1 張）
        feed.on_quotes(&[q(0), q(10), q(70), q(71)]);
        let g0 = tape.gen;
        assert_eq!(tape.apply(), 3, "一幀一次把整批取走");
        assert_eq!(tape.gen, g0 + 1, "整批只讓快取失效一次");
        assert_eq!(tape.apply(), 0, "沒有新成交：不失效、不重畫");
        assert_eq!(tape.all.len(), 3);
        assert_eq!(tape.big.len(), 1);
        assert_eq!(tape.all.newest(0).map(|t| t.qty), Some(1), "最新在最上面");
        tape.toggle_big_only();
        assert_eq!(tape.rows().len(), 1);
        tape.step(false); // 50 → 20：重建大單清單
        assert_eq!(tape.big.len(), 1);
        tape.step(false); // 20 → 10
        assert_eq!(tape.big.len(), 2);
        tape.set_symbol("2317");
        assert!(tape.all.is_empty() && tape.big.is_empty(), "換代號清空");
    }

    #[test]
    fn visible_range_only_covers_rows_on_screen() {
        assert_eq!(TickTape::visible_range(0.0, 200.0, 5000), 0..10);
        assert_eq!(TickTape::visible_range(30.0, 200.0, 5000), 1..12);
        assert_eq!(TickTape::visible_range(0.0, 200.0, 3), 0..3);
        assert_eq!(TickTape::visible_range(0.0, 200.0, 0), 0..0);
    }
}
