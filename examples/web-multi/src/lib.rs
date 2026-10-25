//! 可行性範例：同一個 html 頁面嵌 4 個 egui 元件，每個元件是一個獨立的 `eframe::WebRunner`、各畫在自己的 `<canvas>`。
//!
//! - 報價表、走勢圖直接用桌面版 `../../src/*.rs`（#[path]，同 mobile/ 的做法），程式碼不改。
//! - 行情：每個元件在自己的 wasm 裡跑一份合成行情（`WryFeedHost --demo-ticks`／`xq-demofeed` 同樣的節奏：
//!   每 25 ms 每檔 ±0.15% 隨機走、量加 10–400），**不是真實行情**。不用 WebSocket，頁面打開就會動。
//!   UI 每幀最多套用一次（把這一幀之前累積的 25 ms 步合併成一包再套用）。
//! - JS 端：`start(canvas, kind)` 回傳 `Widget`，可讀 `frames()`、呼叫 `destroy()`、`panic_next_frame()`（驗證隔離用）。
//! - eframe 的 `WebRunner::new()` 每次都會「串接」安裝全域 panic hook：任一個元件 panic，四個 runner 都會被標成已 panic、
//!   一起停。`isolate_panics()` 換成只記錄、不通知其他 runner 的 hook（見 docs/web-multi.md「問題與繞法」）。
#![cfg(target_arch = "wasm32")]
#![allow(dead_code)]

/// 共用程式碼裡的 eprintln! 改寫到 console.log（同桌面版網頁入口）
macro_rules! eprintln {
    ($($t:tt)*) => { $crate::plat::log(&format!($($t)*)) };
}

#[path = "../../../src/chart.rs"]
mod chart;
#[path = "../../../src/drawings.rs"]
mod drawings;
#[path = "../../../src/gfx.rs"]
mod gfx;
#[path = "../../../src/indicators.rs"]
mod indicators;
#[path = "../../../src/layers.rs"]
mod layers;
#[path = "../../../src/model.rs"]
mod model;
#[path = "../../../src/names.rs"]
mod names;
#[path = "../../../src/plat.rs"]
mod plat;
#[path = "../../../src/series.rs"]
mod series;
#[path = "../../../src/store.rs"]
mod store;
#[path = "../../../src/table.rs"]
mod table;
#[path = "../../../src/theme.rs"]
mod theme;
#[path = "../../../src/zstress.rs"]
mod zstress;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use egui::{pos2, vec2, Align2, FontId, Rect};
use wasm_bindgen::prelude::*;
use xq_feed::Quote;

use crate::plat::Instant;
use chart::ChartView;
use series::Period;
use table::QuoteTable;
use theme::*;

/// chart.rs 壓測紀錄會呼叫 crate::rss_mb()
pub fn rss_mb() -> f64 {
    plat::wasm_heap_mb()
}

const TABLE_ROWS: usize = 50;
const STEP: Duration = Duration::from_millis(25);

// ───────────────────────── 合成行情（xq-demofeed 同一套規則） ─────────────────────────

struct Sym {
    symbol: String,
    name: String,
    prev: f64,
    price: f64,
    volume: i64,
}

struct DemoFeed {
    syms: Vec<Sym>,
    rng: u64,
    next: Instant,
    /// 已套用的 25 ms 步數、套用次數（每幀最多一次）
    steps: u64,
    applies: u64,
    order: Vec<String>,
    quotes: HashMap<String, Quote>,
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

impl DemoFeed {
    fn new(list: &[(String, String)], seed: u64) -> Self {
        let syms: Vec<Sym> = list
            .iter()
            .map(|(s, n)| {
                let h = fnv(s);
                let prev = 50.0 + (h % 900) as f64;
                Sym { symbol: s.clone(), name: n.clone(), prev, price: prev, volume: (h % 100_000) as i64 }
            })
            .collect();
        let order = syms.iter().map(|s| s.symbol.clone()).collect();
        let quotes = syms
            .iter()
            .map(|s| (s.symbol.clone(), Quote { symbol: s.symbol.clone(), name: s.name.clone(), price: s.price, change: 0.0, volume: s.volume }))
            .collect();
        DemoFeed { syms, rng: seed | 1, next: Instant::now(), steps: 0, applies: 0, order, quotes }
    }

    fn unit(&mut self) -> f64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 走完到現在為止該走的 25 ms 步（同代號合併，只留最新），回傳是否有新報價；同時回傳下一步還要多久
    fn poll(&mut self) -> (bool, Duration) {
        let now = Instant::now();
        let mut n = 0u32;
        while self.next <= now && n < 40 {
            for i in 0..self.syms.len() {
                let (a, b) = (self.unit(), self.unit());
                let s = &mut self.syms[i];
                let wobble = (a - 0.48) * (s.price * 0.0015).max(0.5);
                s.price = (s.price + wobble).max(0.01);
                s.volume += 10 + (b * 390.0) as i64;
            }
            self.next += STEP;
            n += 1;
        }
        if self.next <= now {
            // 分頁在背景被節流很久：不補跑，直接跳到現在
            self.next = now + STEP;
        }
        if n > 0 {
            for s in &self.syms {
                if let Some(q) = self.quotes.get_mut(&s.symbol) {
                    q.price = (s.price * 100.0).round() / 100.0;
                    q.change = ((s.price - s.prev) * 100.0).round() / 100.0;
                    q.volume = s.volume;
                }
            }
            self.steps += n as u64;
            self.applies += 1;
        }
        (n > 0, self.next.saturating_duration_since(now))
    }
}

// ───────────────────────── 四種元件 ─────────────────────────

enum Body {
    Table { table: Box<QuoteTable>, feed: DemoFeed },
    Chart { chart: Box<ChartView>, feed: DemoFeed },
    Counter,
}

struct Shared {
    frames: Cell<u64>,
    panic_req: Cell<bool>,
    /// 給 JS 叫醒這個元件重畫用
    ctx: RefCell<Option<egui::Context>>,
}

struct WidgetApp {
    kind: String,
    body: Body,
    shared: Rc<Shared>,
}

impl WidgetApp {
    fn new(cc: &eframe::CreationContext<'_>, kind: &str, shared: Rc<Shared>) -> Self {
        setup_fonts(&cc.egui_ctx);
        shared.ctx.replace(Some(cc.egui_ctx.clone()));
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = bg();
        vis.window_fill = panel();
        cc.egui_ctx.set_visuals(vis);
        let seed = fnv(kind) ^ (js_sys::Math::random() * 1e15) as u64;
        let body = match kind {
            "table" => {
                // 50 列：預設自選 33 檔＋補到 50 檔（合成代號）
                let mut list: Vec<(String, String)> = names::WATCH.iter().map(|(s, n)| ((*s).to_string(), (*n).to_string())).collect();
                let mut k = 0;
                while list.len() < TABLE_ROWS {
                    list.push((format!("9{k:03}"), format!("模擬{k:02}")));
                    k += 1;
                }
                list.truncate(TABLE_ROWS);
                let refs: Vec<(&str, &str)> = list.iter().map(|(s, n)| (s.as_str(), n.as_str())).collect();
                Body::Table { table: Box::new(QuoteTable::new(&refs)), feed: DemoFeed::new(&list, seed) }
            }
            "chart" => {
                let (sym, name) = names::WATCH[0];
                Body::Chart {
                    chart: Box::new(ChartView::new(sym, name, Period::Daily)),
                    feed: DemoFeed::new(&[(sym.to_string(), name.to_string())], seed),
                }
            }
            _ => Body::Counter,
        };
        WidgetApp { kind: kind.to_string(), body, shared }
    }
}

impl eframe::App for WidgetApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.shared.panic_req.get() {
            self.shared.panic_req.set(false);
            panic!("[{}] 驗證用：故意 panic", self.kind);
        }
        let n = self.shared.frames.get() + 1;
        self.shared.frames.set(n);
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        match &mut self.body {
            Body::Table { table, feed } => {
                let (fresh, wait) = feed.poll();
                if fresh {
                    table.apply(&feed.order, &feed.quotes);
                }
                table.show(ui, full);
                ctx.request_repaint_after(wait);
            }
            Body::Chart { chart, feed } => {
                let (fresh, wait) = feed.poll();
                if fresh {
                    if let Some(q) = feed.quotes.get(&chart.symbol()) {
                        chart.apply_quote(q.price, q.change, q.volume, &q.name);
                    }
                }
                chart.show(ui, full);
                ctx.request_repaint_after(wait);
            }
            Body::Counter => {
                let p = ui.painter();
                p.rect_filled(full, 0.0, bg());
                p.text(full.center() - vec2(0.0, 14.0), Align2::CENTER_CENTER, format!("{n}"), FontId::monospace(48.0), up());
                p.text(full.center() + vec2(0.0, 30.0), Align2::CENTER_CENTER, "每幀 +1", FontId::proportional(14.0), dim());
                let w = full.width() * ((n % 120) as f32 / 120.0);
                p.rect_filled(Rect::from_min_size(pos2(full.min.x, full.max.y - 4.0), vec2(w, 4.0)), 0.0, accent());
                ctx.request_repaint();
            }
        }
    }
}

/// 網頁版字型（同桌面版 main.rs 的網頁版 setup_fonts）：編進 wasm 的 Noto Sans TC 子集＋等寬 ASCII 子集。
/// `from_static`：四個 runner 共用同一份 wasm 時，字型位元組只有一份（每個 egui Context 各自有字型貼圖）。
fn setup_fonts(ctx: &egui::Context) {
    static TC: &[u8] = include_bytes!("../../../web/fonts/NotoSansTC-subset.otf");
    static MONO: &[u8] = include_bytes!("../../../web/fonts/NotoSansMonoTC-ascii.otf");
    let mut defs = egui::FontDefinitions::default();
    defs.font_data.insert("noto-tc".into(), Arc::new(egui::FontData::from_static(TC)));
    defs.font_data.insert("noto-mono-tc".into(), Arc::new(egui::FontData::from_static(MONO)));
    defs.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "noto-tc".into());
    let mono = defs.families.entry(egui::FontFamily::Monospace).or_default();
    mono.insert(0, "noto-mono-tc".into());
    mono.push("noto-tc".into());
    ctx.set_fonts(defs);
}

// ───────────────────────── JS 介面 ─────────────────────────

/// 一個嵌在頁面裡的 egui 元件（一個 WebRunner）
#[wasm_bindgen]
pub struct Widget {
    runner: eframe::WebRunner,
    shared: Rc<Shared>,
}

#[wasm_bindgen]
impl Widget {
    /// 已畫的幀數（計數器的數字就是這個）
    pub fn frames(&self) -> f64 {
        self.shared.frames.get() as f64
    }
    /// eframe 自己記錄的「這個 runner 是否已 panic」
    pub fn has_panicked(&self) -> bool {
        self.runner.has_panicked()
    }
    /// 下一幀故意 panic（驗證一個元件 panic 會不會拖垮其他元件）
    pub fn panic_next_frame(&self) {
        self.shared.panic_req.set(true);
        if let Some(c) = self.shared.ctx.borrow().as_ref() {
            c.request_repaint();
        }
    }
    /// 關掉這個元件：退訂事件、停 requestAnimationFrame、釋放 WebGL 資源
    pub fn destroy(&self) {
        self.runner.destroy();
    }
}

/// 在指定的 canvas 上啟動一個元件。kind：`table`（報價表 50 列）｜`chart`（K 線，日K）｜`counter`（計數器）
#[wasm_bindgen]
pub async fn start(canvas: web_sys::HtmlCanvasElement, kind: String) -> Result<Widget, JsValue> {
    let shared = Rc::new(Shared { frames: Cell::new(0), panic_req: Cell::new(false), ctx: RefCell::new(None) });
    let runner = eframe::WebRunner::new();
    let s2 = shared.clone();
    runner
        .start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(WidgetApp::new(cc, &kind, s2)))))
        .await?;
    Ok(Widget { runner, shared })
}

/// 繞法：把 eframe 串接的全域 panic hook 換成只寫 console.error 的 hook。
/// 要在所有 `start()` 之後呼叫（之後再 start 的 runner 又會串接一次）。
#[wasm_bindgen]
pub fn isolate_panics() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&format!("PANIC（只停這個元件）: {info}").into());
    }));
}

/// 這個 wasm instance 的線性記憶體大小（bytes）
#[wasm_bindgen]
pub fn wasm_memory_bytes() -> f64 {
    plat::wasm_heap_mb() * 1048576.0
}
