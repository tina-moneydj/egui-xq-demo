//! 背景行情執行緒 → UI 的合併層（與 gpui-xq-demo/src/bridge.rs 同一套合併規則）。
//!
//! 背景執行緒（xq-feed）每收到一包就在 `Inbox` 裡 O(1) 合併：同代號的報價只留最新一筆、
//! 分時快照只留最新、分鐘線依序累積。只有 inbox 從「空」變成「有東西」時才喚醒 UI：
//! `ctx.request_repaint_after(距離上次套用滿 16ms 的剩餘時間)`。UI 每幀最多整包取走一次，
//! 所以不管引擎推多快，UI 最多約每 16ms 套用一次；沒有行情時不會喚醒（不連續重畫）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use crate::plat::Instant;

use parking_lot::Mutex;
use xq_feed::{Bar, Event, Msg, Quote, Stamp, Subscribe};

use crate::tick_tape::TapeFeed;

/// 兩次套用之間至少間隔（≈ 一幀）
pub const APPLY_EVERY: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conn {
    Connecting,
    Up,
    Ready,
    Down,
}

#[derive(Default)]
pub struct Inbox {
    pub quotes: HashMap<String, Quote>,
    /// 插入順序（第一次出現），讓套用順序穩定
    pub order: Vec<String>,
    pub intraday: Option<(String, f64, Vec<Bar>)>,
    pub minutes: Vec<(String, Bar)>,
    pub conn: Option<Conn>,
    /// XQ_TICK_BENCH=1：這批裡最新／最早一包的壓測時間戳
    pub stamp: Option<Stamp>,
    pub stamp_first: Option<Stamp>,
}

pub struct Counters {
    pub frames: AtomicU64,
    pub quote_rows: AtomicU64,
}

pub struct FeedBridge {
    pub inbox: Arc<Mutex<Inbox>>,
    pending: Arc<AtomicBool>,
    /// 上次 UI 套用的時間（相對 epoch 的 µs），背景執行緒用來算喚醒延遲
    last_apply: Arc<AtomicU64>,
    epoch: Instant,
    pub counters: Arc<Counters>,
    cmd: crate::plat::FeedTx,
    last_sub: Option<Subscribe>,
}

impl FeedBridge {
    /// `tape`：有開成交明細面板（`XQ_TAPE=1`）才給；None 時熱路徑完全不多做事。
    pub fn start(ctx: egui::Context, tape: Option<TapeFeed>) -> Self {
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let pending = Arc::new(AtomicBool::new(false));
        let last_apply = Arc::new(AtomicU64::new(0));
        let epoch = Instant::now();
        let counters = Arc::new(Counters { frames: AtomicU64::new(0), quote_rows: AtomicU64::new(0) });
        let cmd_tx = {
            let inbox = inbox.clone();
            let pending = pending.clone();
            let counters = counters.clone();
            let last_apply = last_apply.clone();
            crate::plat::spawn_feed(move |ev| {
                merge(&inbox, &counters, tape.as_ref(), ev);
                if !pending.swap(true, Ordering::AcqRel) {
                    let now = epoch.elapsed().as_micros() as u64;
                    let due = last_apply.load(Ordering::Acquire) + APPLY_EVERY.as_micros() as u64;
                    // 注意：egui 的 delay=0 會排「兩次」重畫（outstanding=1），所以下限 1µs，一包只畫一幀
                    ctx.request_repaint_after(Duration::from_micros(due.saturating_sub(now).max(1)));
                }
            })
        };
        FeedBridge { inbox, pending, last_apply, epoch, counters, cmd: cmd_tx, last_sub: None }
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// 距離可以再套用還要多久（0 = 現在就可以）
    pub fn wait_left(&self) -> Duration {
        let now = self.epoch.elapsed().as_micros() as u64;
        let due = self.last_apply.load(Ordering::Acquire) + APPLY_EVERY.as_micros() as u64;
        Duration::from_micros(due.saturating_sub(now))
    }

    /// UI 端：先清旗標再取走，之後到的包會再喚醒一次。
    pub fn take(&self) -> Inbox {
        self.last_apply.store(self.epoch.elapsed().as_micros() as u64, Ordering::Release);
        self.pending.store(false, Ordering::Release);
        std::mem::take(&mut *self.inbox.lock())
    }

    pub fn subscribe(&mut self, sub: Subscribe) {
        if self.last_sub.as_ref() == Some(&sub) {
            return;
        }
        self.last_sub = Some(sub.clone());
        self.cmd.send(sub);
    }
}

fn merge(inbox: &Mutex<Inbox>, counters: &Counters, tape: Option<&TapeFeed>, ev: Event) {
    let mut ib = inbox.lock();
    match ev {
        Event::Connected => ib.conn = Some(Conn::Up),
        Event::Down => {
            *ib = Inbox { conn: Some(Conn::Down), ..Inbox::default() };
        }
        Event::Stamp(st) => {
            if ib.stamp_first.is_none() {
                ib.stamp_first = Some(st);
            }
            ib.stamp = Some(st);
        }
        Event::Msg(msg) => {
            counters.frames.fetch_add(1, Ordering::Relaxed);
            match msg {
                Msg::Hello { ready } => {
                    if ready {
                        ib.conn = Some(Conn::Ready);
                    }
                }
                Msg::Quotes(quotes) => {
                    counters.quote_rows.fetch_add(quotes.len() as u64, Ordering::Relaxed);
                    // 成交明細要「每一筆」，所以在合併（同代號只留最新）之前先看一眼
                    if let Some(t) = tape {
                        t.on_quotes(&quotes);
                    }
                    for q in quotes {
                        if !ib.quotes.contains_key(&q.symbol) {
                            ib.order.push(q.symbol.clone());
                        }
                        ib.quotes.insert(q.symbol.clone(), q);
                    }
                }
                Msg::Intraday { symbol, prev, bars } => {
                    ib.minutes.retain(|(s, _)| s != &symbol);
                    ib.intraday = Some((symbol, prev, bars));
                }
                Msg::Minutes { symbol, bars } => {
                    for b in bars {
                        if let Some(last) = ib.minutes.last_mut() {
                            if last.0 == symbol && last.1.t == b.t {
                                last.1 = b;
                                continue;
                            }
                        }
                        ib.minutes.push((symbol.clone(), b));
                    }
                }
            }
        }
    }
}
