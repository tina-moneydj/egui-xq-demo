//! 伺服器下發版面（`XQ_LAYOUT_URL` 才開；預設版面與壓測不變）：照 xq-layout 驗證後的 `Screen`
//! 畫頂部選單、切頁，並把內建高頻元件（走勢圖／報價表／資訊格）擺進伺服器指定的分割。
//!
//! - 版面 JSON 在背景抓＋解析（layout_feed），UI 每幀只 take 一次；壞檔沿用上一份，錯誤顯示在狀態列。
//! - 換頁／換版面才重建元件；走勢圖用小池子重用（≤ MAX_CHARTS），不每次 new。
//! - 行情照舊由 `Terminal::apply_feed` 每幀最多套用一次，這裡的 `apply` 只是多套到版面上的元件。
//! - 連動：點報價表或選單的代號 → 所有 link=true 的走勢圖與資訊格換代號。

use egui::{pos2, vec2, Align2, FontId, Rect, Sense, UiBuilder};
use xq_layout::{Action, MenuEntry, Pane, Screen};

use crate::bridge::{Conn, Inbox};
use crate::chart::{ChartEvent, ChartView};
use crate::layout_feed::{self, LayoutFeed};
use crate::series::Period;
use crate::table::QuoteTable;
use crate::theme::*;
use crate::{paint_info, split, status_h, title_h, Info, Stats};

const MENU_H: f32 = 24.0;

enum PaneView {
    Chart { view: Box<ChartView>, link: bool },
    Quotes(Box<QuoteTable>),
    Info,
    Note(String),
}

pub struct LayoutScreen {
    feed: LayoutFeed,
    spec: Option<Screen>,
    page: usize,
    panes: Vec<PaneView>,
    /// 換頁時收回的走勢圖（下次 load 重用；整個 Box 在格子與池子間搬，不重新配置）
    #[allow(clippy::vec_box)]
    spare: Vec<Box<ChartView>>,
    /// 分割計算結果（每幀重用，不配置）
    rects: Vec<xq_layout::Rect>,
    gaps: Vec<xq_layout::Rect>,
    /// 連動代號
    linked: String,
    info: Info,
    error: Option<String>,
    /// 收到幾次新版面（狀態列顯示，方便確認「改 JSON 有生效」）
    loads: u32,
}

/// 名稱：先查版面裡報價表的列（行情會更新），再查內建清單，最後用代號
fn name_of(panes: &[PaneView], sym: &str) -> String {
    for p in panes {
        if let PaneView::Quotes(t) = p {
            if let Some(r) = t.row(sym) {
                return r.name.clone();
            }
        }
    }
    crate::names::WATCH.iter().find(|(s, _)| *s == sym).map(|(_, n)| (*n).to_string()).unwrap_or_else(|| sym.to_string())
}

fn quote_of(panes: &[PaneView], sym: &str) -> Option<(f64, f64, i64)> {
    panes.iter().find_map(|p| match p {
        PaneView::Quotes(t) => t.row(sym).filter(|r| r.price > 0.0).map(|r| (r.price, r.change, r.volume)),
        _ => None,
    })
}

impl LayoutScreen {
    pub fn new(feed: LayoutFeed, symbol: &str, name: &str) -> Self {
        LayoutScreen {
            feed,
            spec: None,
            page: 0,
            panes: Vec::new(),
            spare: Vec::new(),
            rects: Vec::new(),
            gaps: Vec::new(),
            linked: symbol.into(),
            info: Info { symbol: symbol.into(), name: name.into(), price: 0.0, change: 0.0, volume: 0 },
            error: None,
            loads: 0,
        }
    }

    pub fn ready(&self) -> bool {
        self.spec.is_some()
    }

    /// 每幀一次：有新版面就換上（盡量停在同一頁）。回傳 true＝要重新訂閱
    pub fn poll(&mut self) -> bool {
        match self.feed.take() {
            None => false,
            Some(Err(e)) => {
                eprintln!("[layout] {e}");
                self.error = Some(e);
                false
            }
            Some(Ok(s)) => {
                let keep = self.spec.as_ref().and_then(|old| old.pages.get(self.page)).map(|p| p.id.clone());
                self.page = keep.and_then(|id| s.pages.iter().position(|p| p.id == id)).unwrap_or(s.start);
                eprintln!("[layout] 套用新版面：{} 頁、{} 個選單、{} 條警告", s.pages.len(), s.menu.len(), s.warnings.len());
                for w in &s.warnings {
                    eprintln!("[layout] 警告：{w}");
                }
                self.spec = Some(s);
                self.error = None;
                self.loads += 1;
                self.build_page();
                true
            }
        }
    }

    fn build_page(&mut self) {
        let Some(page) = self.spec.as_ref().and_then(|s| s.pages.get(self.page)) else { return };
        for p in self.panes.drain(..) {
            if let PaneView::Chart { view, .. } = p {
                self.spare.push(view);
            }
        }
        self.spare.truncate(xq_layout::MAX_CHARTS);
        // 先建報價表（走勢圖要用它查名稱／現價）
        let mut panes: Vec<PaneView> = page
            .panes
            .iter()
            .map(|p| match p {
                Pane::Quotes { title, symbols, columns } => {
                    let rows = symbols
                        .iter()
                        .map(|(s, n)| (s.clone(), if n.is_empty() { name_of(&[], s) } else { n.clone() }))
                        .collect();
                    PaneView::Quotes(Box::new(QuoteTable::with_layout(rows, title, columns)))
                }
                Pane::Info => PaneView::Info,
                Pane::Unsupported(t) => PaneView::Note(t.clone()),
                Pane::Chart { .. } => PaneView::Note(String::new()),
            })
            .collect();
        // 這頁第一張連動圖若有指定代號，連動代號跟著它
        if let Some(sym) = page.panes.iter().find_map(|p| match p {
            Pane::Chart { symbol, link: true, .. } if !symbol.is_empty() => Some(symbol.clone()),
            _ => None,
        }) {
            self.linked = sym;
        }
        for (i, p) in page.panes.iter().enumerate() {
            if let Pane::Chart { symbol, period, link } = p {
                let sym = if *link || symbol.is_empty() { self.linked.clone() } else { symbol.clone() };
                let name = name_of(&panes, &sym);
                let per = Period::from_code(period).unwrap_or(Period::Intraday);
                let view = match self.spare.pop() {
                    Some(mut v) => {
                        v.load(&sym, &name, per, quote_of(&panes, &sym));
                        v
                    }
                    None => Box::new(ChartView::new_ephemeral(&sym, &name, per)),
                };
                panes[i] = PaneView::Chart { view, link: *link };
            }
        }
        self.panes = panes;
        let sym = self.linked.clone();
        self.select_linked();
        self.info = Info { name: name_of(&self.panes, &sym), symbol: sym, price: 0.0, change: 0.0, volume: 0 };
    }

    /// 報價表的選取列跟著連動代號（表裡沒有就不動）
    fn select_linked(&mut self) {
        for p in &mut self.panes {
            if let PaneView::Quotes(t) = p {
                t.select_symbol(&self.linked);
            }
        }
    }

    fn set_linked(&mut self, sym: &str) {
        if sym == self.linked {
            return;
        }
        self.linked = sym.into();
        let name = name_of(&self.panes, sym);
        let quote = quote_of(&self.panes, sym);
        for p in &mut self.panes {
            if let PaneView::Chart { view, link: true } = p {
                let per = view.period();
                view.load(sym, &name, per, quote);
            }
        }
        self.select_linked();
        let (pr, c, v) = quote.unwrap_or((0.0, 0.0, 0));
        self.info = Info { symbol: sym.into(), name, price: pr, change: c, volume: v };
    }

    /// 訂閱內容：這頁所有報價表的代號＋走勢圖代號（去重，上限 500）；引擎的走勢圖＝連動代號
    pub fn subscription(&self) -> xq_feed::Subscribe {
        let mut symbols: Vec<String> = Vec::new();
        let mut add = |s: &str| {
            if symbols.len() < 500 && !symbols.iter().any(|x| x == s) {
                symbols.push(s.to_string());
            }
        };
        add(&self.linked);
        let mut period = None;
        for p in &self.panes {
            match p {
                PaneView::Quotes(t) => t.symbols().iter().for_each(|s| add(s)),
                PaneView::Chart { view, link } => {
                    add(&view.symbol());
                    if *link && period.is_none() {
                        period = Some(view.period());
                    }
                }
                _ => {}
            }
        }
        xq_feed::Subscribe { symbols, chart: self.linked.clone(), period: period.unwrap_or(Period::Intraday).feed_code().into() }
    }

    /// 由 `Terminal::apply_feed` 呼叫（每幀最多一次）
    pub fn apply(&mut self, ib: &Inbox) {
        for p in &mut self.panes {
            match p {
                PaneView::Quotes(t) => {
                    if !ib.quotes.is_empty() {
                        t.apply(&ib.order, &ib.quotes);
                    }
                }
                PaneView::Chart { view, .. } => {
                    let sym = view.symbol();
                    if let Some(q) = ib.quotes.get(&sym) {
                        let name = if q.name.is_empty() || q.name == q.symbol { String::new() } else { q.name.clone() };
                        view.apply_quote(q.price, q.change, q.volume, &name);
                    }
                    if let Some((s, prev, bars)) = &ib.intraday {
                        view.set_intraday(s, *prev, bars);
                    }
                    for (s, bar) in &ib.minutes {
                        view.apply_minute(s, bar);
                    }
                }
                _ => {}
            }
        }
        if let Some(q) = ib.quotes.get(&self.linked) {
            self.info.price = q.price;
            self.info.change = q.change;
            self.info.volume = q.volume;
        }
    }

    pub fn tokens_changed(&mut self) {
        for p in &mut self.panes {
            match p {
                PaneView::Chart { view, .. } => view.tokens_changed(),
                PaneView::Quotes(t) => t.tokens_changed(),
                _ => {}
            }
        }
    }

    /// 畫整個視窗（標題列、頂部選單、各格、狀態列）。回傳 true＝要重新訂閱
    pub fn show(&mut self, ui: &mut egui::Ui, stats: &Stats, conn: Conn) -> bool {
        let full = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0.0, bg());
        let title = Rect::from_min_size(full.min, vec2(full.width(), title_h()));
        let menu_r = Rect::from_min_size(pos2(full.min.x, title.max.y), vec2(full.width(), MENU_H));
        let status = Rect::from_min_max(pos2(full.min.x, full.max.y - status_h()), full.max);
        let body = Rect::from_min_max(pos2(full.min.x, menu_r.max.y + 1.0), pos2(full.max.x, status.min.y));
        let f12 = FontId::proportional(tk().font.md);
        let f11 = FontId::proportional(tk().font.sm);

        // 標題列：程式名＋伺服器給的標題＋目前頁
        painter.rect_filled(title, 0.0, header());
        painter.hline(title.x_range(), title.max.y - 0.5, (1.0, border()));
        let mut x = title.min.x + 12.0;
        let (st, pt) = match &self.spec {
            Some(s) => (if s.title.is_empty() { "伺服器下發版面" } else { s.title.as_str() }, s.pages.get(self.page).map_or("", |p| p.title.as_str())),
            None => ("伺服器下發版面", ""),
        };
        for (s, c) in [("XQ", up()), (st, text()), (pt, accent())] {
            if !s.is_empty() {
                x += painter.text(pos2(x, title.center().y), Align2::LEFT_CENTER, s, f12.clone(), c).width() + 12.0;
            }
        }

        // 頂部選單（伺服器下發）
        painter.rect_filled(menu_r, 0.0, panel());
        painter.hline(menu_r.x_range(), menu_r.max.y + 0.5, (1.0, border()));
        let mut act: Option<Action> = None;
        if let Some(spec) = &self.spec {
            let mut mui = ui.new_child(UiBuilder::new().max_rect(menu_r.shrink2(vec2(6.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            egui::MenuBar::new().ui(&mut mui, |ui| {
                for e in &spec.menu {
                    menu_entry(ui, e, &mut act);
                }
            });
        }

        // 各格
        let mut resub = false;
        let mut pick: Option<String> = None;
        if let Some(page) = self.spec.as_ref().and_then(|s| s.pages.get(self.page)) {
            let b = xq_layout::Rect::new(body.min.x, body.min.y, body.width(), body.height());
            xq_layout::solve(&page.root, b, split(), &mut self.rects, &mut self.gaps);
            for g in &self.gaps {
                painter.rect_filled(Rect::from_min_size(pos2(g.x, g.y), vec2(g.w, g.h)), 0.0, border());
            }
            for (i, p) in self.panes.iter_mut().enumerate() {
                let Some(r) = self.rects.get(i).filter(|r| r.w >= 1.0 && r.h >= 1.0) else { continue };
                let r = Rect::from_min_size(pos2(r.x, r.y), vec2(r.w, r.h));
                ui.push_id(("xq-pane", self.page, i), |ui| match p {
                    PaneView::Chart { view, link } => {
                        if let Some(ChartEvent::Period(per)) = view.show(ui, r) {
                            let (sym, name) = (view.symbol(), view.m.name.clone());
                            view.load(&sym, &name, per, None);
                            resub |= *link;
                        }
                    }
                    PaneView::Quotes(t) => {
                        if let Some(s) = t.show(ui, r) {
                            pick = Some(s);
                        }
                    }
                    PaneView::Info => paint_info(&painter, r, &self.info, stats),
                    PaneView::Note(t) => {
                        let pa = painter.with_clip_rect(r);
                        pa.rect_filled(r, 0.0, panel());
                        pa.text(r.center(), Align2::CENTER_CENTER, t.as_str(), f12.clone(), dim());
                    }
                });
            }
        } else {
            let msg = match &self.error {
                Some(e) => format!("還沒拿到版面：{e}"),
                None => format!("等待版面伺服器… {}", self.feed.source),
            };
            painter.text(body.center(), Align2::CENTER_CENTER, msg, f12.clone(), dim());
        }

        match act {
            Some(Action::Page(i)) => {
                if i != self.page {
                    self.page = i;
                    self.build_page();
                    resub = true;
                }
            }
            Some(Action::Symbol(s)) => {
                self.set_linked(&s);
                resub = true;
            }
            Some(Action::Url(u)) => layout_feed::open_url(&u),
            None => {}
        }
        if let Some(s) = pick {
            self.set_linked(&s);
            resub = true;
        }

        // 狀態列：連線＋版面來源／警告／錯誤（滑過看警告全文）
        painter.rect_filled(status, 0.0, header());
        painter.hline(status.x_range(), status.min.y + 0.5, (1.0, border()));
        let (conn_text, conn_color) = match conn {
            Conn::Connecting => ("連線中…", dim()),
            Conn::Up => ("已連線，等待行情", accent()),
            Conn::Ready => ("行情連線", down()),
            Conn::Down => ("行情中斷，重連中", up()),
        };
        let w = painter.text(pos2(status.min.x + 12.0, status.center().y), Align2::LEFT_CENTER, format!("● {conn_text}"), f11.clone(), conn_color).width();
        let nwarn = self.spec.as_ref().map_or(0, |s| s.warnings.len());
        let (lt, lc) = match (&self.error, nwarn) {
            (Some(e), _) => (format!("{e}（沿用上一份版面）"), up()),
            (None, 0) => (format!("版面：{}（第 {} 版）", self.feed.source, self.loads), dim()),
            (None, n) => (format!("版面：{}（第 {} 版）· ⚠ {n} 條警告", self.feed.source, self.loads), accent()),
        };
        let lr = painter.text(pos2(status.min.x + 28.0 + w, status.center().y), Align2::LEFT_CENTER, lt, f11, lc);
        if let Some(s) = self.spec.as_ref().filter(|s| !s.warnings.is_empty()) {
            ui.interact(lr, ui.scope_id().with("layout-warn"), Sense::hover()).on_hover_text(s.warnings.join("\n"));
        }
        let m11 = FontId::monospace(tk().font.sm);
        let mut rx = status.max.x - 12.0;
        for t in [format!("{} {:.1} MB", if crate::plat::IS_WEB { "WASM" } else { "RSS" }, stats.rss_mb), format!("FPS {}", stats.fps)] {
            rx -= painter.text(pos2(rx, status.center().y), Align2::RIGHT_CENTER, t, m11.clone(), text()).width() + 16.0;
        }
        resub
    }
}

fn menu_entry(ui: &mut egui::Ui, e: &MenuEntry, act: &mut Option<Action>) {
    match e {
        MenuEntry::Group { label, items } => {
            ui.menu_button(label.as_str(), |ui| {
                for it in items {
                    menu_entry(ui, it, act);
                }
            });
        }
        MenuEntry::Item { label, action } => {
            if ui.button(label.as_str()).clicked() {
                *act = Some(action.clone());
            }
        }
        MenuEntry::Separator => {
            ui.separator();
        }
    }
}
