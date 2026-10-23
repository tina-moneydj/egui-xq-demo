//! XQ 風格看盤 demo 手機版（Android，egui／eframe，NativeActivity）。
//!
//! 行情、報價表、走勢圖、指標、畫線、圖層、壓測程式碼全部直接用桌面版 `../src/*.rs`（#[path]），
//! 只有版面換成手機單一畫面：上＝狀態列＋搜尋框，中間＝報價表（5 萬列虛擬化、手指 fling）或走勢圖（點一列進入），
//! 下＝封包／套用計數。觸控：走勢圖單指平移、長按十字線、雙指縮放（chart.rs 的 handle_touch）。
//! 設定：`adb shell setprop debug.xq.e0 KEY=VAL`（見 droid.rs），桌面版的 XQ_* 壓測開關原樣可用。
#![cfg(target_os = "android")]
#![allow(dead_code)]

#[path = "../../src/bridge.rs"]
mod bridge;
#[path = "../../src/chart.rs"]
mod chart;
mod droid;
#[path = "../../src/drawings.rs"]
mod drawings;
#[path = "../../src/gfx.rs"]
mod gfx;
#[path = "../../src/indicators.rs"]
mod indicators;
#[path = "../../src/layers.rs"]
mod layers;
#[path = "../../src/model.rs"]
mod model;
#[path = "../../src/names.rs"]
mod names;
#[path = "../../src/series.rs"]
mod series;
#[path = "../../src/store.rs"]
mod store;
#[path = "../../src/table.rs"]
mod table;
#[path = "../../src/theme.rs"]
mod theme;
#[path = "../../src/tick_tape.rs"]
mod tick_tape;
#[path = "../../src/tickbench.rs"]
mod tickbench;
#[path = "../../src/zstress.rs"]
mod zstress;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, FontId, Rect, Sense};

use bridge::{Conn, FeedBridge};
use chart::{ChartEvent, ChartView};
use series::Period;
use table::QuoteTable;
use theme::*;

const ONE_US: Duration = Duration::from_micros(1);
const TOP_H: f32 = 34.0;
const SEARCH_H: f32 = 40.0;
const BACK_H: f32 = 34.0;
const STATUS_H: f32 = 24.0;

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    List,
    Chart,
}

#[derive(Default, Clone)]
struct Stats {
    fps: u32,
    rss_mb: f64,
    frames_in: u64,
    quote_rows_in: u64,
    applies: u32,
    apply_us_avg: u64,
}

enum Group {
    Wait(Instant),
    Settle(Instant),
    Tick { t0: Instant, a: GSnap, next: Instant },
    Scroll { s0: Instant, b: GSnap, a: GSnap, last_tick: Instant, next: Instant },
    Done,
}
type GSnap = (u64, u64, u64, u64, usize);
struct GroupCfg {
    tick_ms: u64,
    scroll_ms: u64,
    hz: u64,
    batch: usize,
    rng: u64,
}

struct MobileApp {
    bridge: FeedBridge,
    table: QuoteTable,
    chart: ChartView,
    conn: Conn,
    screen: Screen,
    search: String,
    search_id: Option<egui::Id>,
    frames: u32,
    total_frames: u64,
    applies: u32,
    apply_us: u64,
    last_frames_in: u64,
    last_rows_in: u64,
    last_sample: Instant,
    stats: Stats,
    metrics_log: bool,
    flog: droid::FrameLog,
    ind: droid::IndDelta,
    bench: bool,
    bench_frame: u8,
    bench_quote: u8,
    track_jank: bool,
    last_frame: Option<Instant>,
    jank_n: u64,
    jank_ms: f64,
    tick: Option<tickbench::TickBench>,
    group: Option<(GroupCfg, Group)>,
}

impl MobileApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = BG;
        vis.window_fill = PANEL;
        vis.extreme_bg_color = PANEL;
        cc.egui_ctx.set_visuals(vis);
        let (sym, name) = names::WATCH[0];
        let bridge = FeedBridge::start(cc.egui_ctx.clone(), None);
        let mut table = QuoteTable::new(names::WATCH);
        let period = std::env::var("XQ_PERIOD").ok().and_then(|v| Period::from_code(&v)).unwrap_or(Period::Intraday);
        #[allow(unused_mut)]
        let mut chart = ChartView::new(sym, name, period);
        if droid::ind_bench_reps() > 0 {
            let (b, v) = chart.m.ind_microbench(droid::ind_bench_reps());
            droid::ind_bench_report(b, v);
        }
        let tick = xq_feed::tick_bench_enabled().then(tickbench::TickBench::default);
        if tick.is_some() {
            table.enable_stress_indexed();
        }
        let group = std::env::var_os("XQ_GROUP_STRESS").is_some_and(|v| v != "0").then(|| {
            let num = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0).unwrap_or(d);
            let hz = num("XQ_STRESS_TICK_HZ", 3000);
            (
                GroupCfg { tick_ms: num("XQ_STRESS_TICK_MS", 2000), scroll_ms: num("XQ_STRESS_SCROLL_MS", 2000), hz, batch: ((hz as f64) / 30.0).round().max(1.0) as usize, rng: 0x9E37_79B9_7F4A_7C15 },
                Group::Wait(Instant::now()),
            )
        });
        let screen = if chart.stress_on() || std::env::var("XQ_SCREEN").is_ok_and(|v| v == "chart") { Screen::Chart } else { Screen::List };
        if std::env::var_os("XQ_STRESS_LIST").is_some_and(|v| v != "0") && !table.stress {
            table.toggle_stress();
        }
        let mut t = MobileApp {
            bridge,
            table,
            chart,
            conn: Conn::Connecting,
            screen,
            search: String::new(),
            search_id: None,
            frames: 0,
            total_frames: 0,
            applies: 0,
            apply_us: 0,
            last_frames_in: 0,
            last_rows_in: 0,
            last_sample: Instant::now(),
            stats: Stats::default(),
            metrics_log: std::env::var_os("XQ_METRICS").is_some(),
            flog: droid::FrameLog::default(),
            ind: droid::IndDelta::default(),
            bench: std::env::var_os("XQ_BENCH").is_some_and(|v| v != "0"),
            bench_frame: 0,
            bench_quote: 0,
            track_jank: false,
            last_frame: None,
            jank_n: 0,
            jank_ms: 0.0,
            tick,
            group,
        };
        t.resubscribe();
        t
    }

    fn resubscribe(&mut self) {
        let sub = xq_feed::Subscribe { symbols: self.table.symbols(), chart: self.chart.symbol(), period: self.chart.period().feed_code().into() };
        self.bridge.subscribe(sub);
    }

    fn switch_chart(&mut self, symbol: String, period: Option<Period>) {
        let (name, quote) = match self.table.row(&symbol) {
            Some(r) => (r.name.clone(), (r.price > 0.0).then_some((r.price, r.change, r.volume))),
            None => (symbol.clone(), None),
        };
        let period = period.unwrap_or_else(|| self.chart.period());
        self.chart.load(&symbol, &name, period, quote);
        self.resubscribe();
    }

    fn apply_feed(&mut self) {
        let t0 = Instant::now();
        let ib = self.bridge.take();
        if let Some(c) = ib.conn {
            self.conn = c;
        }
        let mut changed = 0;
        if !ib.quotes.is_empty() {
            self.conn = Conn::Ready;
            changed = self.table.apply(&ib.order, &ib.quotes);
            let sym = self.chart.symbol();
            if let Some(q) = ib.quotes.get(&sym) {
                let name = if q.name.is_empty() || q.name == q.symbol { self.table.row(&sym).map(|r| r.name.clone()).unwrap_or_default() } else { q.name.clone() };
                self.chart.apply_quote(q.price, q.change, q.volume, &name);
            }
        }
        if let Some((sym, prev, bars)) = &ib.intraday {
            self.chart.set_intraday(sym, *prev, bars);
        }
        for (sym, bar) in &ib.minutes {
            self.chart.apply_minute(sym, bar);
        }
        self.applies += 1;
        let us = t0.elapsed().as_micros() as u64;
        self.apply_us += us;
        if let Some(tb) = self.tick.as_mut() {
            tb.on_apply(ib.stamp_first, ib.stamp, ib.quotes.len(), changed, us);
        }
    }

    fn sample(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let frames_in = self.bridge.counters.frames.load(Relaxed);
        let rows_in = self.bridge.counters.quote_rows.load(Relaxed);
        self.stats = Stats {
            fps: self.frames,
            rss_mb: rss_mb(),
            frames_in: frames_in - self.last_frames_in,
            quote_rows_in: rows_in - self.last_rows_in,
            applies: self.applies,
            apply_us_avg: if self.applies > 0 { self.apply_us / self.applies as u64 } else { 0 },
        };
        if self.metrics_log {
            let s = &self.stats;
            self.flog.report(&(format!(",\"rssMb\":{:.1},\"rowsIn\":{},\"applies\":{},\"applyUs\":{},\"screen\":\"{}\"", s.rss_mb, s.quote_rows_in, s.applies, s.apply_us_avg, if self.screen == Screen::Chart { "chart" } else { "list" }) + &self.ind.json(model::ind_counters())));
        }
        if let Some(tb) = self.tick.as_mut() {
            tb.report(rows_in, frames_in, self.stats.fps, self.stats.rss_mb);
        }
        self.last_frames_in = frames_in;
        self.last_rows_in = rows_in;
        self.frames = 0;
        self.applies = 0;
        self.apply_us = 0;
    }

    fn gsnap(&self) -> GSnap {
        (self.total_frames, self.table.paints, self.table.paint_us, self.table.cells, self.table.len())
    }

    /// 與桌面版 XQ_GROUP_STRESS 同流程（1.2 秒後開 5 萬列 → 只跳價 → 邊跳價邊正弦捲動），印 GROUPPERF|{json}
    fn group_step(&mut self, now: Instant) -> Option<Duration> {
        let (mut cfg, st) = self.group.take()?;
        fn stats(a: GSnap, b: GSnap, secs: f64) -> String {
            let paints = b.1 - a.1;
            let avg_ms = if paints > 0 { (b.2 - a.2) as f64 / paints as f64 / 1000.0 } else { 0.0 };
            let cells = if paints > 0 { (b.3 - a.3) / paints } else { 0 };
            format!("\"fps\":{:.1},\"paints\":{},\"avgPaintMs\":{:.3},\"avgCellsPerPaint\":{},\"cellUpdates\":{}", (b.0 - a.0) as f64 / secs, paints, avg_ms, cells, b.3 - a.3)
        }
        let (next, wake) = match st {
            Group::Wait(t) if now.duration_since(t) >= Duration::from_millis(1200) => {
                if !self.table.stress {
                    self.table.toggle_stress();
                }
                (Group::Settle(now), Some(Duration::from_millis(150)))
            }
            s @ Group::Wait(t) => (s, Some(Duration::from_millis(1200).saturating_sub(now.duration_since(t)))),
            Group::Settle(t) if now.duration_since(t) >= Duration::from_millis(150) => {
                let a = self.gsnap();
                eprintln!("GROUPPERF|{{\"phase\":\"tick-start\",\"rows\":{},\"cols\":6,\"tickHz\":{}}}", a.4, cfg.hz);
                (Group::Tick { t0: now, a, next: now }, Some(Duration::ZERO))
            }
            s @ Group::Settle(_) => (s, Some(Duration::from_millis(5))),
            Group::Tick { t0, a, next } => {
                if now.duration_since(t0) >= Duration::from_millis(cfg.tick_ms) {
                    let secs = now.duration_since(t0).as_secs_f64();
                    let b = self.gsnap();
                    eprintln!("GROUPPERF|{{\"phase\":\"tick-end\",\"elapsedMs\":{:.0},{}}}", secs * 1000.0, stats(a, b, secs));
                    self.track_jank = true;
                    self.last_frame = None;
                    self.jank_n = 0;
                    self.jank_ms = 0.0;
                    (Group::Scroll { s0: now, b, a, last_tick: now, next: now }, Some(Duration::ZERO))
                } else if now >= next {
                    self.table.synth_ticks(cfg.batch, &mut cfg.rng);
                    (Group::Tick { t0, a, next: now + Duration::from_millis(33) }, Some(Duration::from_millis(33)))
                } else {
                    (Group::Tick { t0, a, next }, Some(next - now))
                }
            }
            Group::Scroll { s0, b, a, mut last_tick, next } => {
                let el = now.duration_since(s0);
                if el >= Duration::from_millis(cfg.scroll_ms) {
                    let secs = el.as_secs_f64();
                    self.track_jank = false;
                    let c = self.gsnap();
                    let (jn, jms) = (self.jank_n, self.jank_ms);
                    eprintln!(
                        "GROUPPERF|{{\"phase\":\"done\",\"rows\":{},\"cols\":6,\"tickFps\":{:.1},\"scrollFps\":{:.1},\"scrollJankCount\":{jn},\"scrollJankAvgMs\":{:.1},{}}}",
                        c.4,
                        (b.0 - a.0) as f64 / (cfg.tick_ms as f64 / 1000.0),
                        (c.0 - b.0) as f64 / secs,
                        if jn > 0 { jms / jn as f64 } else { 0.0 },
                        stats(b, c, secs)
                    );
                    (Group::Done, None)
                } else if now >= next {
                    let f = el.as_secs_f64() / (cfg.scroll_ms as f64 / 1000.0);
                    let frac = (f * std::f64::consts::PI * 4.0).sin() * 0.5 + 0.5;
                    self.table.scroll_frac(frac);
                    if now.duration_since(last_tick) >= Duration::from_millis(33) {
                        last_tick = now;
                        self.table.synth_ticks(cfg.batch, &mut cfg.rng);
                    }
                    (Group::Scroll { s0, b, a, last_tick, next: now + Duration::from_millis(16) }, Some(Duration::from_millis(16)))
                } else {
                    (Group::Scroll { s0, b, a, last_tick, next }, Some(next - now))
                }
            }
            Group::Done => (Group::Done, None),
        };
        self.group = Some((cfg, next));
        wake
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0.0, BG);
        let f12 = FontId::proportional(12.0);
        let m11 = FontId::monospace(11.0);

        // ── 狀態列（上）──
        let top = Rect::from_min_size(full.min, vec2(full.width(), TOP_H));
        painter.rect_filled(top, 0.0, HEADER);
        let mut x = top.min.x + 10.0;
        for (s, c) in [("XQ", UP), ("egui 手機版", TEXT)] {
            x += painter.text(pos2(x, top.center().y), Align2::LEFT_CENTER, s, FontId::proportional(14.0), c).width() + 8.0;
        }
        let (conn_text, conn_color) = match self.conn {
            Conn::Connecting => ("連線中", DIM),
            Conn::Up => ("等待行情", ACCENT),
            Conn::Ready => ("行情連線", DOWN),
            Conn::Down => ("重連中", UP),
        };
        painter.text(pos2(x, top.center().y), Align2::LEFT_CENTER, format!("● {conn_text}"), f12.clone(), conn_color);
        let s = self.stats.clone();
        painter.text(pos2(top.max.x - 10.0, top.center().y), Align2::RIGHT_CENTER, format!("FPS {}  RSS {:.0} MB", s.fps, s.rss_mb), m11.clone(), TEXT);

        // ── 搜尋框 ──
        let sr = Rect::from_min_size(pos2(full.min.x, top.max.y), vec2(full.width(), SEARCH_H));
        painter.rect_filled(sr, 0.0, PANEL);
        let mut su = ui.new_child(egui::UiBuilder::new().max_rect(sr.shrink2(vec2(8.0, 5.0))));
        let te = egui::TextEdit::singleline(&mut self.search)
            .id_salt("search")
            .hint_text("搜尋代號／名稱（例：2330、台積）")
            .font(FontId::proportional(15.0))
            .desired_width(sr.width() - 16.0);
        self.search_id = Some(su.add(te).id);

        // ── 主體 ──
        let status = Rect::from_min_max(pos2(full.min.x, full.max.y - STATUS_H), full.max);
        let body = Rect::from_min_max(pos2(full.min.x, sr.max.y), pos2(full.max.x, status.min.y));
        match self.screen {
            Screen::List => {
                if let Some(sym) = self.table.show(ui, body) {
                    if let Some(tb) = self.tick.as_mut() {
                        tb.on_switch(&sym);
                    }
                    self.switch_chart(sym, None);
                    // 壓測（高頻進價）時留在報價表，量的是「點選→畫面」；一般使用點一列進走勢圖
                    if self.tick.is_none() {
                        self.screen = Screen::Chart;
                    }
                    ctx.request_repaint_after(ONE_US);
                }
            }
            Screen::Chart => {
                let br = Rect::from_min_size(body.min, vec2(body.width(), BACK_H));
                painter.rect_filled(br, 0.0, HEADER);
                let back = ui.interact(Rect::from_min_size(br.min, vec2(96.0, BACK_H)), ui.scope_id().with("back"), Sense::click());
                painter.text(pos2(br.min.x + 10.0, br.center().y), Align2::LEFT_CENTER, "‹ 報價", FontId::proportional(15.0), ACCENT);
                if back.clicked() {
                    self.screen = Screen::List;
                }
                let cr = Rect::from_min_max(pos2(body.min.x, br.max.y), body.max);
                if let Some(ChartEvent::Period(p)) = self.chart.show(ui, cr) {
                    let sym = self.chart.symbol();
                    self.switch_chart(sym, Some(p));
                }
            }
        }

        // ── 搜尋結果（浮在主體上）──
        if !self.search.trim().is_empty() {
            let hits = self.table.search(&self.search, 12);
            let rh = 40.0;
            let rr = Rect::from_min_size(body.min, vec2(body.width(), rh * hits.len().max(1) as f32));
            let lp = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("search-hits")));
            lp.rect_filled(rr, 0.0, PANEL);
            if hits.is_empty() {
                lp.text(pos2(rr.min.x + 12.0, rr.min.y + rh / 2.0), Align2::LEFT_CENTER, "找不到", FontId::proportional(14.0), DIM);
            }
            let mut pick = None;
            for (k, (sym, name)) in hits.iter().enumerate() {
                let r = Rect::from_min_size(pos2(rr.min.x, rr.min.y + k as f32 * rh), vec2(rr.width(), rh));
                let resp = ui.interact(r, ui.scope_id().with(("hit", k)), Sense::click());
                lp.hline(r.x_range(), r.max.y - 0.5, (1.0, BORDER));
                lp.text(pos2(r.min.x + 12.0, r.center().y), Align2::LEFT_CENTER, sym, FontId::monospace(15.0), TEXT);
                lp.text(pos2(r.min.x + 90.0, r.center().y), Align2::LEFT_CENTER, name, FontId::proportional(15.0), TEXT);
                if let Some(row) = self.table.row(sym) {
                    if row.price > 0.0 {
                        lp.text(pos2(r.max.x - 12.0, r.center().y), Align2::RIGHT_CENTER, format!("{:.2}", row.price), FontId::monospace(14.0), dir_color(row.change));
                    }
                }
                if resp.clicked() {
                    pick = Some(sym.clone());
                }
            }
            if let Some(sym) = pick {
                self.table.select_symbol(&sym);
                self.switch_chart(sym, None);
                self.search.clear();
                self.screen = Screen::Chart;
                if let Some(id) = self.search_id {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            }
        }

        // ── 狀態列（下）──
        painter.rect_filled(status, 0.0, HEADER);
        painter.hline(status.x_range(), status.min.y + 0.5, (1.0, BORDER));
        painter.text(pos2(status.min.x + 10.0, status.center().y), Align2::LEFT_CENTER, format!("{}", xq_feed::feed_addr()), m11.clone(), DIM);
        painter.text(
            pos2(status.max.x - 10.0, status.center().y),
            Align2::RIGHT_CENTER,
            format!("封包 {}/s → 套用 {}/s · {} 列", s.frames_in, s.applies, self.table.len()),
            m11,
            DIM,
        );
    }
}

impl eframe::App for MobileApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        self.frames += 1;
        self.total_frames += 1;
        self.flog.frame();
        if self.track_jank {
            if let Some(prev) = self.last_frame.replace(now) {
                let dt = now.duration_since(prev).as_secs_f64() * 1000.0;
                if dt > 32.0 {
                    self.jank_n += 1;
                    self.jank_ms += dt;
                }
            }
        }
        if let Some(tb) = self.tick.as_mut() {
            tb.on_frame_start();
        }
        if self.bench_frame == 1 {
            self.bench_frame = 2;
            eprintln!("BENCH first-frame");
        }
        if self.bench_quote == 1 {
            self.bench_quote = 2;
            eprintln!("BENCH first-quote");
        }
        let mut wake: Option<Duration> = None;
        let mut want = |d: Duration| wake = Some(wake.map_or(d, |w: Duration| w.min(d)));
        if self.chart.stress_on() {
            self.chart.stress_frame(self.total_frames);
        }
        if let Some(d) = self.group_step(now) {
            want(d);
        }
        if self.bridge.pending() {
            let left = self.bridge.wait_left();
            if left.is_zero() {
                self.apply_feed();
            } else {
                want(left);
            }
        }
        if now.duration_since(self.last_sample) >= Duration::from_secs(1) {
            self.last_sample = now;
            self.sample();
        }
        want(Duration::from_secs(1).saturating_sub(now.duration_since(self.last_sample)));

        self.draw(ui);

        if self.bench {
            if self.bench_frame == 0 {
                self.bench_frame = 1;
                ctx.request_repaint_after(ONE_US);
            }
            if self.bench_quote == 0 && self.conn == Conn::Ready {
                self.bench_quote = 1;
                ctx.request_repaint_after(ONE_US);
            }
        }
        if self.tick.as_ref().is_some_and(|t| t.has_pending()) {
            want(bridge::APPLY_EVERY);
        }
        if self.chart.stress_on() {
            want(Duration::from_micros(16_667).saturating_sub(now.elapsed()));
        }
        if let Some(d) = wake {
            ctx.request_repaint_after(d.max(ONE_US));
        }
    }
}

/// 中文字型：mmap Android 系統的 Noto Sans CJK（/system/fonts/NotoSansCJK-Regular.ttc，index 3 = TC），不複製進記憶體。
/// Android 沒有 Mono CJK，等寬字也用同一個字面（數字仍是等寬：Noto CJK 的數字是 tabular）。
fn setup_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let path = std::env::var("XQ_CJK_FONT").unwrap_or_else(|_| "/system/fonts/NotoSansCJK-Regular.ttc".into());
    let index = std::env::var("XQ_CJK_INDEX").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    match std::fs::File::open(&path).and_then(|f| unsafe { memmap2::Mmap::map(&f) }) {
        Ok(mm) => {
            let blob: egui::epaint::text::Blob = Arc::new(mm);
            defs.font_data.insert("noto-tc".into(), Arc::new(egui::FontData::from_blob(blob, index)));
            defs.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "noto-tc".into());
            defs.families.entry(egui::FontFamily::Monospace).or_default().push("noto-tc".into());
        }
        Err(e) => eprintln!("[fonts] 讀不到 {path}: {e}（中文會變方塊）"),
    }
    ctx.set_fonts(defs);
}

pub fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok()))
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

#[no_mangle]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    droid::init(app.internal_data_path());
    eprintln!("[egui] android_main renderer=glow");
    let opts = eframe::NativeOptions { android_app: Some(app), renderer: eframe::Renderer::Glow, ..Default::default() };
    if let Err(e) = eframe::run_native("XQ egui", opts, Box::new(|cc| Ok(Box::new(MobileApp::new(cc))))) {
        eprintln!("[egui] run_native error: {e}");
    }
}
