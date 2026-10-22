//! XQ 風格看盤 demo（Rust + egui／eframe 立即模式），接 WryFeedHost（127.0.0.1:47631）行情。
//!
//! 版面與 gpui-xq-demo 相同：上＝走勢圖、左下＝報價表、右下＝資訊格（不嵌 WebView），分隔線可拖曳。
//! 效能：行情在背景執行緒解碼＋合併（xq-feed / bridge），UI 每幀最多套用一次（≥16ms 一次）；
//! 沒有行情、沒有輸入時不重畫（只有每秒一次的狀態列取樣）。走勢圖三層與報價表可見列都快取成 Mesh，
//! 沒變就沿用；但 egui 每次重畫都是整個視窗重新送給 GPU（立即模式、沒有局部重畫）。

#![allow(dead_code)] // series/indicators/model 由 GPUI 版原樣移植，保留未用的 API
mod bridge;
mod chart;
mod drawings;
mod gfx;
mod indicators;
mod layers;
mod model;
mod names;
mod series;
mod store;
mod table;
mod theme;
mod tick_tape;
mod tickbench;
mod zstress;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense};

use bridge::{Conn, FeedBridge};
use chart::{ChartEvent, ChartView};
use series::Period;
use table::QuoteTable;
use tick_tape::{TapeFeed, TickTape};
use theme::*;

/// egui 的 request_repaint()（delay=0）會多排一幀「讓東西穩定」；我們的狀態是自己管的，用 1µs 避開那一幀
const ONE_US: Duration = Duration::from_micros(1);
const TITLE_H: f32 = 26.0;
const STATUS_H: f32 = 22.0;
const SPLIT: f32 = 5.0;

#[derive(Default, Clone)]
struct Stats {
    fps: u32,
    rss_mb: f64,
    frames_in: u64,
    quote_rows_in: u64,
    applies: u32,
    apply_us_avg: u64,
    chart_paints: u64,
    chart_paint_us_avg: u64,
}

struct Info {
    symbol: String,
    name: String,
    price: f64,
    change: f64,
    volume: i64,
}

/// XQ_GROUP_STRESS：與 gpui 版 spawn_stress 同流程（1.2 秒後開 5 萬列 → 只跳價 → 邊跳價邊正弦捲動）
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

/// XQ_WINDOWS=N 的其他視窗：只有自己的走勢圖／資訊格／分隔線位置；報價表、行情連線、合併都共用主視窗的
struct Peer {
    id: egui::ViewportId,
    title: String,
    chart: ChartView,
    info: Info,
    top_frac: f32,
    left_w: f32,
    frames: u32,
    tick: Option<tickbench::TickBench>,
}

impl Peer {
    fn apply(&mut self, ib: &bridge::Inbox, table: &QuoteTable) {
        let sym = self.chart.symbol();
        if let Some(q) = ib.quotes.get(&sym) {
            let name = if q.name.is_empty() || q.name == q.symbol {
                table.row(&sym).map(|r| r.name.clone()).unwrap_or_default()
            } else {
                q.name.clone()
            };
            self.chart.apply_quote(q.price, q.change, q.volume, &name);
            self.info.price = q.price;
            self.info.change = q.change;
            self.info.volume = q.volume;
        }
        if let Some((sym, prev, bars)) = &ib.intraday {
            self.chart.set_intraday(sym, *prev, bars);
        }
        for (sym, bar) in &ib.minutes {
            self.chart.apply_minute(sym, bar);
        }
    }
}

fn window_count() -> usize {
    std::env::var("XQ_WINDOWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1usize).clamp(1, 8)
}

struct Terminal {
    peers: Vec<Peer>,
    debug_repaint: bool,
    /// XQ_DEBUG：印滑鼠按下（與 gpui 版 on_any_mouse_down 相同）
    debug: bool,
    bridge: FeedBridge,
    table: QuoteTable,
    chart: ChartView,
    info: Info,
    /// 成交明細範例面板（XQ_TAPE=1 才開；預設 None，版面與壓測不變）
    tape: Option<TickTape>,
    conn: Conn,
    top_frac: f32,
    left_w: f32,
    frames: u32,
    total_frames: u64,
    applies: u32,
    apply_us: u64,
    last_frames_in: u64,
    last_rows_in: u64,
    last_paints: u64,
    last_paint_us: u64,
    last_sample: Instant,
    stats: Stats,
    metrics_log: bool,
    bench: bool,
    /// 0 = 還沒畫、1 = 這幀畫了（下一幀開始時印）、2 = 已印
    bench_frame: u8,
    bench_quote: u8,
    track_jank: bool,
    last_frame: Option<Instant>,
    jank_n: u64,
    jank_ms: f64,
    tick: Option<tickbench::TickBench>,
    group: Option<(GroupCfg, Group)>,
    frame_t: Instant,
}

impl Terminal {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = BG;
        vis.window_fill = PANEL;
        cc.egui_ctx.set_visuals(vis);
        let (sym, name) = names::WATCH[0];
        let tape_feed = std::env::var_os("XQ_TAPE").is_some_and(|v| v != "0").then(|| TapeFeed::new(sym));
        let tape = tape_feed.clone().map(|f| TickTape::new(f, sym, &cc.egui_ctx));
        let bridge = FeedBridge::start(cc.egui_ctx.clone(), tape_feed);
        let mut table = QuoteTable::new(names::WATCH);
        let period = std::env::var("XQ_PERIOD").ok().and_then(|v| Period::from_code(&v)).unwrap_or(Period::Intraday);
        let chart = ChartView::new(sym, name, period);
        let tick = xq_feed::tick_bench_enabled().then(tickbench::TickBench::default);
        if tick.is_some() {
            table.enable_stress_indexed();
        }
        let group = std::env::var_os("XQ_GROUP_STRESS").is_some_and(|v| v != "0").then(|| {
            let num = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0).unwrap_or(d);
            let hz = num("XQ_STRESS_TICK_HZ", 3000);
            (
                GroupCfg {
                    tick_ms: num("XQ_STRESS_TICK_MS", 2000),
                    scroll_ms: num("XQ_STRESS_SCROLL_MS", 2000),
                    hz,
                    batch: ((hz as f64) / 30.0).round().max(1.0) as usize,
                    rng: 0x9E37_79B9_7F4A_7C15,
                },
                Group::Wait(Instant::now()),
            )
        });
        let peers = (1..window_count())
            .map(|k| {
                let (sym, name) = names::WATCH[k % names::WATCH.len()];
                Peer {
                    id: egui::ViewportId::from_hash_of(("xq-window", k)),
                    title: format!("XQ 風格測試（egui） #{}", k + 1),
                    chart: ChartView::new(sym, name, period),
                    info: Info { symbol: sym.into(), name: name.into(), price: 0.0, change: 0.0, volume: 0 },
                    top_frac: std::env::var("XQ_TOP_FRAC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.56),
                    left_w: 470.0,
                    frames: 0,
                    tick: xq_feed::tick_bench_enabled().then(|| tickbench::TickBench::for_window(k)),
                }
            })
            .collect();
        let mut t = Terminal {
            peers,
            debug_repaint: std::env::var("XQ_DEBUG_REPAINT").is_ok_and(|v| v == "1"),
            debug: std::env::var_os("XQ_DEBUG").is_some(),
            bridge,
            table,
            chart,
            info: Info { symbol: sym.into(), name: name.into(), price: 0.0, change: 0.0, volume: 0 },
            tape,
            conn: Conn::Connecting,
            top_frac: std::env::var("XQ_TOP_FRAC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.56),
            left_w: 470.0,
            frames: 0,
            total_frames: 0,
            applies: 0,
            apply_us: 0,
            last_frames_in: 0,
            last_rows_in: 0,
            last_paints: 0,
            last_paint_us: 0,
            last_sample: Instant::now(),
            stats: Stats::default(),
            metrics_log: std::env::var_os("XQ_METRICS").is_some(),
            bench: std::env::var_os("XQ_BENCH").is_some_and(|v| v != "0"),
            bench_frame: 0,
            bench_quote: 0,
            track_jank: false,
            last_frame: None,
            jank_n: 0,
            jank_ms: 0.0,
            tick,
            group,
            frame_t: Instant::now(),
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
        if let Some(t) = self.tape.as_mut() {
            t.set_symbol(&symbol);
        }
        let (p, c, v) = quote.unwrap_or((0.0, 0.0, 0));
        self.info = Info { symbol: symbol.clone(), name, price: p, change: c, volume: v };
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
                let name = if q.name.is_empty() || q.name == q.symbol {
                    self.table.row(&sym).map(|r| r.name.clone()).unwrap_or_default()
                } else {
                    q.name.clone()
                };
                self.chart.apply_quote(q.price, q.change, q.volume, &name);
                self.info.price = q.price;
                self.info.change = q.change;
                self.info.volume = q.volume;
            }
        }
        if let Some((sym, prev, bars)) = &ib.intraday {
            self.chart.set_intraday(sym, *prev, bars);
        }
        for (sym, bar) in &ib.minutes {
            self.chart.apply_minute(sym, bar);
        }
        if let Some(t) = self.tape.as_mut() {
            // 和報價表同一個節奏（每幀最多一次）；沒有新成交就不讓快取失效
            t.apply();
        }
        self.applies += 1;
        let us = t0.elapsed().as_micros() as u64;
        self.apply_us += us;
        if let Some(tb) = self.tick.as_mut() {
            tb.on_apply(ib.stamp_first, ib.stamp, ib.quotes.len(), changed, us);
        }
        for p in self.peers.iter_mut() {
            let t1 = Instant::now();
            p.apply(&ib, &self.table);
            let us = us + t1.elapsed().as_micros() as u64;
            if let Some(tb) = p.tick.as_mut() {
                tb.on_apply(ib.stamp_first, ib.stamp, ib.quotes.len(), changed, us);
            }
        }
    }

    fn sample(&mut self) {
        let frames_in = self.bridge.counters.frames.load(std::sync::atomic::Ordering::Relaxed);
        let rows_in = self.bridge.counters.quote_rows.load(std::sync::atomic::Ordering::Relaxed);
        let (paints, paint_us) = self.chart.paint_totals();
        let dp = paints - self.last_paints;
        self.stats = Stats {
            fps: self.frames,
            rss_mb: rss_mb(),
            frames_in: frames_in - self.last_frames_in,
            quote_rows_in: rows_in - self.last_rows_in,
            applies: self.applies,
            apply_us_avg: if self.applies > 0 { self.apply_us / self.applies as u64 } else { 0 },
            chart_paints: dp,
            chart_paint_us_avg: if dp > 0 { (paint_us - self.last_paint_us) / dp } else { 0 },
        };
        if self.metrics_log {
            let s = &self.stats;
            eprintln!(
                "[metrics] fps={} rss_mb={:.1} feed_frames/s={} quote_rows/s={} applies/s={} apply_us_avg={} chart_paints/s={} chart_paint_us_avg={} conn={:?}",
                s.fps, s.rss_mb, s.frames_in, s.quote_rows_in, s.applies, s.apply_us_avg, s.chart_paints, s.chart_paint_us_avg, self.conn
            );
        }
        if let Some(tb) = self.tick.as_mut() {
            tb.report(rows_in, frames_in, self.stats.fps, self.stats.rss_mb);
        }
        for p in self.peers.iter_mut() {
            if let Some(tb) = p.tick.as_mut() {
                tb.report(rows_in, frames_in, p.frames, self.stats.rss_mb);
            }
            p.frames = 0;
        }
        self.last_frames_in = frames_in;
        self.last_rows_in = rows_in;
        self.last_paints = paints;
        self.last_paint_us = paint_us;
        self.frames = 0;
        self.applies = 0;
        self.apply_us = 0;
    }

    fn gsnap(&self) -> GSnap {
        (self.total_frames, self.table.paints, self.table.paint_us, self.table.cells, self.table.len())
    }

    /// 回傳下一次要在多久後被叫醒
    fn group_step(&mut self, now: Instant) -> Option<Duration> {
        let (mut cfg, st) = self.group.take()?;
        fn stats(a: GSnap, b: GSnap, secs: f64) -> String {
            let paints = b.1 - a.1;
            let avg_ms = if paints > 0 { (b.2 - a.2) as f64 / paints as f64 / 1000.0 } else { 0.0 };
            let cells = if paints > 0 { (b.3 - a.3) / paints } else { 0 };
            format!(
                "\"fps\":{:.1},\"paints\":{},\"avgPaintMs\":{:.3},\"avgCellsPerPaint\":{},\"cellUpdates\":{}",
                (b.0 - a.0) as f64 / secs, paints, avg_ms, cells, b.3 - a.3
            )
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
                    let n = now + Duration::from_millis(33);
                    (Group::Tick { t0, a, next: n }, Some(Duration::from_millis(33)))
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
}

impl eframe::App for Terminal {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.debug_repaint {
            // XQ_DEBUG_REPAINT=1：印這一幀是被誰叫醒的（找多餘重畫用）
            let c = ui.ctx().repaint_causes();
            if !c.is_empty() {
                eprintln!("[repaint] {:?}", c.iter().map(|c| format!("{c}")).collect::<Vec<_>>());
            }
        }
        let ctx = ui.ctx().clone();
        if self.debug {
            ctx.input(|i| {
                for e in &i.events {
                    if let egui::Event::PointerButton { pos, button, pressed: true, .. } = e {
                        eprintln!("[debug] mouse down {button:?} at {pos:?}");
                    }
                }
            });
        }
        let now = Instant::now();
        self.frame_t = now;
        self.frames += 1;
        self.total_frames += 1;
        if self.track_jank {
            if let Some(prev) = self.last_frame.replace(now) {
                let dt = now.duration_since(prev).as_secs_f64() * 1000.0;
                if dt > 32.0 {
                    self.jank_n += 1;
                    self.jank_ms += dt;
                }
            }
        }
        // 上一幀已經畫上螢幕：記 present
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

        // ── 版面 ──
        let (period, pick) = draw(
            ui,
            View {
                chart: &mut self.chart,
                info: &self.info,
                table: &mut self.table,
                tape: self.tape.as_mut(),
                stats: &self.stats,
                conn: self.conn,
                top_frac: &mut self.top_frac,
                left_w: &mut self.left_w,
            },
        );
        if let Some(p) = period {
            let sym = self.chart.symbol();
            self.switch_chart(sym, Some(p));
        }
        if let Some(sym) = pick {
            if let Some(tb) = self.tick.as_mut() {
                tb.on_switch(&sym);
            }
            self.switch_chart(sym, None);
            ctx.request_repaint_after(ONE_US);
        }

        // ── XQ_WINDOWS 的其他視窗（immediate viewport：跟主視窗同一幀、同一份報價表）──
        if !self.peers.is_empty() {
            self.show_peers(&ctx);
        }

        // ── 量測與排程 ──
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
        if self.tick.as_ref().is_some_and(|t| t.has_pending())
            || self.peers.iter().any(|p| p.tick.as_ref().is_some_and(|t| t.has_pending()))
        {
            want(bridge::APPLY_EVERY);
        }
        if self.chart.stress_on() {
            // 壓測：連續重畫（與 GPUI on_next_frame 迴圈相同），上限 60 fps
            want(Duration::from_micros(16_667).saturating_sub(now.elapsed()));
        }
        if let Some(d) = wake {
            ctx.request_repaint_after(d.max(ONE_US));
        }
    }
}

impl Terminal {
    fn show_peers(&mut self, ctx: &egui::Context) {
        let Terminal { peers, table, stats, conn, .. } = self;
        let (stats, conn) = (&*stats, *conn);
        for p in peers.iter_mut() {
            let builder = egui::ViewportBuilder::default().with_inner_size([1240.0, 760.0]).with_title(p.title.clone());
            ctx.show_viewport_immediate(p.id, builder, |ui, _class| {
                p.frames += 1;
                if let Some(tb) = p.tick.as_mut() {
                    tb.on_frame_start();
                }
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    ui.ctx().send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
                }
                // 每個視窗自己的 id 範圍（選單、子圖層等 egui id 不和主視窗撞）
                let (period, pick) = ui.push_id(p.id, |ui| draw(
                    ui,
                    View {
                        chart: &mut p.chart,
                        info: &p.info,
                        table,
                        tape: None,
                        stats,
                        conn,
                        top_frac: &mut p.top_frac,
                        left_w: &mut p.left_w,
                    },
                )).inner;
                if let Some(sym) = pick.clone() {
                    if let Some(tb) = p.tick.as_mut() {
                        tb.on_switch(&sym);
                    }
                }
                let sym = pick.or_else(|| period.map(|_| p.chart.symbol()));
                if let Some(sym) = sym {
                    let (name, quote) = match table.row(&sym) {
                        Some(r) => (r.name.clone(), (r.price > 0.0).then_some((r.price, r.change, r.volume))),
                        None => (sym.clone(), None),
                    };
                    let per = period.unwrap_or_else(|| p.chart.period());
                    p.chart.load(&sym, &name, per, quote);
                    let (pr, c, v) = quote.unwrap_or((0.0, 0.0, 0));
                    p.info = Info { symbol: sym, name, price: pr, change: c, volume: v };
                }
            });
        }
    }
}

struct View<'a> {
    chart: &'a mut ChartView,
    info: &'a Info,
    table: &'a mut QuoteTable,
    tape: Option<&'a mut TickTape>,
    stats: &'a Stats,
    conn: Conn,
    top_frac: &'a mut f32,
    left_w: &'a mut f32,
}

/// 一個看盤視窗的版面（主視窗與 XQ_WINDOWS 的其他視窗共用）。回傳（切週期, 點選代號）
fn draw(ui: &mut egui::Ui, v: View<'_>) -> (Option<Period>, Option<String>) {
    let View { chart, info, table, tape, stats, conn, top_frac, left_w } = v;
    let ctx = ui.ctx().clone();
    let full = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(full, 0.0, BG);
    let title = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
    let status = Rect::from_min_max(pos2(full.min.x, full.max.y - STATUS_H), full.max);
    let avail = full.height() - TITLE_H - STATUS_H - SPLIT;
    let top_h = (avail * *top_frac).floor();
    let chart_r = Rect::from_min_size(pos2(full.min.x, title.max.y), vec2(full.width(), top_h));
    let hsplit = Rect::from_min_size(pos2(full.min.x, chart_r.max.y), vec2(full.width(), SPLIT));
    let bottom = Rect::from_min_max(pos2(full.min.x, hsplit.max.y), pos2(full.max.x, status.min.y));
    let lw = left_w.min(full.width() - 200.0).max(200.0);
    let table_r = Rect::from_min_size(bottom.min, vec2(lw, bottom.height()));
    let vsplit = Rect::from_min_size(pos2(table_r.max.x, bottom.min.y), vec2(SPLIT, bottom.height()));
    let mut info_r = Rect::from_min_max(pos2(vsplit.max.x, bottom.min.y), bottom.max);
    // 成交明細：右下多一欄（分隔線 + 276px），與 gpui 版相同
    let tape_r = tape.as_ref().map(|_| {
        let tr = Rect::from_min_max(pos2(bottom.max.x - tick_tape::PANEL_W, bottom.min.y), bottom.max);
        info_r.max.x = tr.min.x - SPLIT;
        tr
    });

    // 標題列
    painter.rect_filled(title, 0.0, HEADER);
    painter.hline(title.x_range(), title.max.y - 0.5, (1.0, BORDER));
    let f12 = FontId::proportional(12.0);
    let mut x = title.min.x + 12.0;
    for (s, c) in [("XQ", UP), ("風格測試 · egui 立即模式版", TEXT), ("同一個 WryFeedHost 行情（demo-ticks 為合成測試資料）", DIM)] {
        x += painter.text(pos2(x, title.center().y), Align2::LEFT_CENTER, s, f12.clone(), c).width() + 12.0;
    }

    // 走勢圖
    let period = match chart.show(ui, chart_r) {
        Some(ChartEvent::Period(p)) => Some(p),
        None => None,
    };

    // 分隔線（可拖曳）
    let hs = ui.interact(hsplit, ui.scope_id().with("hsplit"), Sense::drag());
    let vs = ui.interact(vsplit, ui.scope_id().with("vsplit"), Sense::drag());
    if hs.hovered() || hs.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if vs.hovered() || vs.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if hs.dragged() {
        if let Some(q) = hs.interact_pointer_pos() {
            *top_frac = ((q.y - full.min.y - TITLE_H) / avail).clamp(0.15, 0.85);
        }
    }
    if vs.dragged() {
        if let Some(q) = vs.interact_pointer_pos() {
            *left_w = (q.x - full.min.x).clamp(200.0, full.width() - 200.0);
        }
    }
    painter.rect_filled(hsplit, 0.0, if hs.hovered() || hs.dragged() { ACCENT } else { BORDER });
    painter.rect_filled(vsplit, 0.0, if vs.hovered() || vs.dragged() { ACCENT } else { BORDER });

    // 報價表
    let pick = table.show(ui, table_r);

    // 資訊格
    paint_info(&painter, info_r, info, stats);
    if let (Some(t), Some(tr)) = (tape, tape_r) {
        painter.rect_filled(Rect::from_min_max(pos2(info_r.max.x, tr.min.y), pos2(tr.min.x, tr.max.y)), 0.0, BORDER);
        t.show(ui, tr);
    }

    // 狀態列
    painter.rect_filled(status, 0.0, HEADER);
    painter.hline(status.x_range(), status.min.y + 0.5, (1.0, BORDER));
    let (conn_text, conn_color) = match conn {
        Conn::Connecting => ("連線中…", DIM),
        Conn::Up => ("已連線，等待行情", ACCENT),
        Conn::Ready => ("行情連線", DOWN),
        Conn::Down => ("行情中斷，重連中", UP),
    };
    let f11 = FontId::proportional(11.0);
    let m11 = FontId::monospace(11.0);
    painter.text(pos2(status.min.x + 12.0, status.center().y), Align2::LEFT_CENTER, format!("● {conn_text} {}", xq_feed::feed_addr()), f11, conn_color);
    let s = stats;
    let mut rx = status.max.x - 12.0;
    for (t, c) in [
        (format!("封包 {}/s → 套用 {}/s", s.frames_in, s.applies), DIM),
        (format!("RSS {:.1} MB", s.rss_mb), TEXT),
        (format!("FPS {}", s.fps), TEXT),
    ] {
        rx -= painter.text(pos2(rx, status.center().y), Align2::RIGHT_CENTER, t, m11.clone(), c).width() + 16.0;
    }
    (period, pick)
}

fn paint_info(painter: &egui::Painter, r: Rect, i: &Info, s: &Stats) {
        let painter = painter.with_clip_rect(r);
        painter.rect_filled(r, 0.0, PANEL);
        let hr = Rect::from_min_size(r.min, vec2(r.width(), 26.0));
        painter.rect_filled(hr, 0.0, HEADER);
        painter.hline(hr.x_range(), hr.max.y - 0.5, (1.0, BORDER));
        let f12 = FontId::proportional(12.0);
        let m12 = FontId::monospace(12.0);
        painter.text(pos2(hr.min.x + 8.0, hr.center().y), Align2::LEFT_CENTER, "資訊", f12.clone(), TEXT);
        let has = i.price > 0.0;
        let color = dir_color(i.change);
        let x0 = r.min.x + 12.0;
        let mut y = hr.max.y + 12.0;
        painter.text(pos2(x0, y + 8.0), Align2::LEFT_CENTER, format!("{} {}", i.symbol, i.name), FontId::proportional(15.0), TEXT);
        y += 24.0;
        let kv = |y: f32, x: f32, w: f32, k: &str, v: String, c: Color32| {
            painter.text(pos2(x, y + 10.0), Align2::LEFT_CENTER, k, f12.clone(), DIM);
            painter.text(pos2(x + w, y + 10.0), Align2::RIGHT_CENTER, v, m12.clone(), c);
        };
        let rows = [
            ("成交", if has { format!("{:.2}", i.price) } else { "--".into() }, color),
            ("漲跌", if has { fmt_change(i.change) } else { "--".into() }, color),
            ("幅度", if has { format!("{:+.2}%", pct(i.price, i.change)) } else { "--".into() }, color),
            ("昨收", if has { format!("{:.2}", i.price - i.change) } else { "--".into() }, TEXT),
            ("總量", if has { fmt_int(i.volume) } else { "--".into() }, TEXT),
        ];
        for (k, (a, b, c)) in rows.into_iter().enumerate() {
            kv(y + k as f32 * 20.0, x0, 220.0, a, b, c);
        }
        let x1 = x0 + 236.0;
        let y1 = hr.max.y + 12.0;
        painter.text(pos2(x1, y1 + 8.0), Align2::LEFT_CENTER, "效能（每秒取樣）", FontId::proportional(13.0), DIM);
        let perf = [
            ("引擎封包", format!("{}/s", s.frames_in)),
            ("報價筆數（未合併）", format!("{}/s", fmt_int(s.quote_rows_in as i64))),
            ("UI 套用（合併後）", format!("{}/s", s.applies)),
            ("套用平均耗時", format!("{} µs", s.apply_us_avg)),
            ("走勢圖重建", format!("{}/s · {} µs", s.chart_paints, s.chart_paint_us_avg)),
        ];
        for (k, (a, b)) in perf.into_iter().enumerate() {
            kv(y1 + 24.0 + k as f32 * 20.0, x1, 240.0, a, b, TEXT);
        }
        painter.text(
            pos2(x0, y + 5.0 * 20.0 + 16.0),
            Align2::LEFT_CENTER,
            "右下網頁格：egui 版不內嵌 WebView，此格先做資訊／效能面板。",
            FontId::proportional(11.0),
            DIM,
        );
}

/// 中文字型：直接 mmap 系統的 Noto Sans CJK（.ttc 一份檔案兩個字面：TC＝index 3、Mono TC＝index 8），不複製進記憶體
fn setup_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let path = std::env::var("XQ_CJK_FONT").unwrap_or_else(|_| "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into());
    match std::fs::File::open(&path).and_then(|f| unsafe { memmap2::Mmap::map(&f) }) {
        Ok(mm) => {
            let blob: egui::epaint::text::Blob = Arc::new(mm);
            defs.font_data.insert("noto-tc".into(), Arc::new(egui::FontData::from_blob(blob.clone(), 3)));
            defs.font_data.insert("noto-mono-tc".into(), Arc::new(egui::FontData::from_blob(blob, 8)));
            defs.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "noto-tc".into());
            defs.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "noto-mono-tc".into());
        }
        Err(e) => eprintln!("[fonts] 讀不到 {path}: {e}（中文會變方塊）"),
    }
    ctx.set_fonts(defs);
}

pub fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok())
        })
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn main() -> eframe::Result {
    let renderer = match std::env::var("XQ_RENDERER").as_deref() {
        Ok("wgpu") => eframe::Renderer::Wgpu,
        _ => eframe::Renderer::Glow,
    };
    eprintln!("[egui] renderer={renderer:?}");
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1240.0, 760.0]).with_title("XQ 風格測試（egui）"),
        renderer,
        ..Default::default()
    };
    eframe::run_native("XQ 風格測試（egui）", opts, Box::new(|cc| Ok(Box::new(Terminal::new(cc)))))
}
