//! egui 弱項驗證用的獨立小程式（不影響主程式）：`cargo run --release --example probe`
//!
//! 三個分頁對應 docs/egui-weakness-check.md 的第 1～3 項：
//! - 字與輸入法：各字級的中文、數字樣張（可切字型、hinting、縮放）＋搜尋框／多行框的輸入法事件與選字窗位置
//! - 元件外觀：只用設計 token 畫的報價卡、下單匣（圓角、細線、hover、焦點框、停用、淡入提示）
//! - 多視窗：開關多個原生視窗、各自關閉、選單／提示／對話框碰到視窗邊緣的行為、把面板移到新視窗
//!
//! 環境變數（自動化測試用）：
//! - `XQ_PROBE_TAB=cjk|visual|multi`：啟動時的分頁
//! - `XQ_PROBE_WINDOWS=N`：啟動就開 N 個子視窗（1..=16）
//! - `XQ_IME_LOG=1`：輸入法事件與選字窗應在的位置（螢幕座標，實體像素）印到 stderr，格式 `[ime] ...`
//! - `XQ_CJK_FONT`／`XQ_CJK_FONT_INDEX`：系統字型檔與字面（例如 Mac 的 PingFang.ttc index 2）
//! - `XQ_CJK_BOLD`：粗體字型檔（Windows 預設 msjhbd.ttc；Linux 預設 Noto Sans CJK Bold）

use std::sync::Arc;
use std::time::Instant;

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Rect, RichText, Sense, Stroke, StrokeKind};

fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}
fn tk() -> &'static xq_tokens::Theme {
    xq_tokens::t()
}
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

// ───────────── 字型 ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FontSrc {
    /// 網頁版內嵌的 Noto Sans TC 子集（三個平台同一份，方便對照）
    Embedded,
    /// 系統字型（XQ_CJK_FONT 或各 OS 預設）
    System,
}

/// 預設系統字型：(路徑, ttc 字面)
fn system_font() -> Option<(String, u32)> {
    if let Some(p) = env("XQ_CJK_FONT") {
        let idx = env("XQ_CJK_FONT_INDEX").and_then(|v| v.parse().ok()).unwrap_or(0);
        return Some((p, idx));
    }
    let list: &[(&str, u32)] = &[
        (r"C:\Windows\Fonts\msjh.ttc", 0),                              // 微軟正黑體
        ("/System/Library/Fonts/PingFang.ttc", 2),                     // 舊版 macOS 的蘋方 TC
        ("/System/Library/Fonts/STHeiti Medium.ttc", 1),               // 華文黑體（新版 macOS 會導到蘋方）
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 3), // Noto Sans CJK TC
    ];
    list.iter().find(|(p, _)| std::path::Path::new(p).exists()).map(|(p, i)| (p.to_string(), *i))
}

fn bold_font() -> Option<(String, u32)> {
    if let Some(p) = env("XQ_CJK_BOLD") {
        return Some((p, env("XQ_CJK_BOLD_INDEX").and_then(|v| v.parse().ok()).unwrap_or(0)));
    }
    let list: &[(&str, u32)] = &[
        (r"C:\Windows\Fonts\msjhbd.ttc", 0),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc", 3),
    ];
    list.iter().find(|(p, _)| std::path::Path::new(p).exists()).map(|(p, i)| (p.to_string(), *i))
}

/// 回傳實際用到的字型說明（顯示在畫面上，也印到 stderr）
fn setup_fonts(ctx: &egui::Context, src: FontSrc) -> String {
    static TC: &[u8] = include_bytes!("../web/fonts/NotoSansTC-subset.otf");
    static MONO: &[u8] = include_bytes!("../web/fonts/NotoSansMonoTC-ascii.otf");
    let mut defs = egui::FontDefinitions::default();
    let mut desc = String::new();
    let sys = if src == FontSrc::System { system_font() } else { None };
    match sys.as_ref().and_then(|(p, i)| std::fs::read(p).ok().map(|b| (p, *i, b))) {
        Some((p, i, blob)) => {
            let mut fd = egui::FontData::from_owned(blob);
            fd.index = i;
            defs.font_data.insert("cjk".into(), Arc::new(fd));
            desc.push_str(&format!("系統字型 {p} #{i}"));
        }
        None => {
            defs.font_data.insert("cjk".into(), Arc::new(egui::FontData::from_static(TC)));
            if src == FontSrc::System {
                desc.push_str("（找不到系統字型，改用）");
            }
            desc.push_str("內嵌 Noto Sans TC 子集");
        }
    }
    defs.font_data.insert("mono".into(), Arc::new(egui::FontData::from_static(MONO)));
    defs.families.entry(FontFamily::Proportional).or_default().insert(0, "cjk".into());
    let m = defs.families.entry(FontFamily::Monospace).or_default();
    m.insert(0, "mono".into());
    m.insert(1, "cjk".into());
    // egui 沒有字重（FontId 只有大小＋family）：粗體要另外載一個字型檔當成一個 family
    let bold = FontFamily::Name("bold".into());
    match bold_font().and_then(|(p, i)| std::fs::read(&p).ok().map(|b| (p, i, b))) {
        Some((p, i, blob)) => {
            let mut fd = egui::FontData::from_owned(blob);
            fd.index = i;
            defs.font_data.insert("bold".into(), Arc::new(fd));
            defs.families.insert(bold, vec!["bold".into(), "cjk".into()]);
            desc.push_str(&format!("；粗體 {p}"));
        }
        None => {
            defs.families.insert(bold, vec!["cjk".into()]);
            desc.push_str("；沒有粗體字型（粗體會跟一般字一樣）");
        }
    }
    ctx.set_fonts(defs);
    eprintln!("[fonts] {desc}");
    desc
}

// ───────────── 多視窗 ─────────────

struct Win {
    id: egui::ViewportId,
    n: u32,
    frames: u64,
    opened: Instant,
    first_frame_ms: Option<f64>,
    /// 從主視窗「移過來」的面板（撕下面板的替代做法）
    panel: Option<PanelState>,
    modal: bool,
    want_close: bool,
}

#[derive(Clone, Default)]
struct PanelState {
    note: String,
    clicks: u32,
}

fn panel_ui(ui: &mut egui::Ui, p: &mut PanelState) {
    ui.label(RichText::new("可搬移的面板").color(rgb(tk().ui.text_dim)));
    ui.horizontal(|ui| {
        if ui.button("按我").clicked() {
            p.clicks += 1;
        }
        ui.label(format!("按了 {} 次", p.clicks));
    });
    ui.add(egui::TextEdit::singleline(&mut p.note).hint_text("面板裡的文字（搬移後保留）"));
}

// ───────────── App ─────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Cjk,
    Visual,
    Multi,
}

struct Probe {
    tab: Tab,
    font_src: FontSrc,
    font_desc: String,
    hinting: bool,
    zoom: f32,
    search: String,
    memo: String,
    ime_log: Vec<String>,
    log_ime: bool,
    last_ime_rect: Option<Rect>,
    // 元件外觀
    qty: String,
    price: String,
    toast: Option<Instant>,
    side_buy: bool,
    // 多視窗
    wins: Vec<Win>,
    next_win: u32,
    main_panel: Option<PanelState>,
    frames: u64,
}

impl Probe {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font_src = if env("XQ_CJK_FONT").is_some() { FontSrc::System } else { FontSrc::Embedded };
        let font_desc = setup_fonts(&cc.egui_ctx, font_src);
        apply_visuals(&cc.egui_ctx);
        let tab = match env("XQ_PROBE_TAB").as_deref() {
            Some("visual") => Tab::Visual,
            Some("multi") => Tab::Multi,
            _ => Tab::Cjk,
        };
        let mut p = Probe {
            tab,
            font_src,
            font_desc,
            hinting: true,
            zoom: 1.0,
            search: String::new(),
            memo: String::new(),
            ime_log: Vec::new(),
            log_ime: env("XQ_IME_LOG").is_some(),
            last_ime_rect: None,
            qty: "1".into(),
            price: "1,085.00".into(),
            toast: None,
            side_buy: true,
            wins: Vec::new(),
            next_win: 1,
            main_panel: Some(PanelState::default()),
            frames: 0,
        };
        let n = env("XQ_PROBE_WINDOWS").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0).min(16);
        for _ in 0..n {
            p.open_win(None);
        }
        p
    }

    fn open_win(&mut self, panel: Option<PanelState>) {
        let n = self.next_win;
        self.next_win += 1;
        self.wins.push(Win {
            id: egui::ViewportId::from_hash_of(("probe-win", n)),
            n,
            frames: 0,
            opened: Instant::now(),
            first_frame_ms: None,
            panel,
            modal: false,
            want_close: false,
        });
        eprintln!("[mw] open {n}");
    }
}

fn apply_visuals(ctx: &egui::Context) {
    let mut v = if xq_tokens::mode() == xq_tokens::Mode::Light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = rgb(tk().ui.bg);
    v.window_fill = rgb(tk().ui.panel);
    v.override_text_color = Some(rgb(tk().ui.text));
    v.selection.bg_fill = rgb(tk().ui.selected);
    v.widgets.noninteractive.bg_stroke = Stroke::new(tk().stroke.hairline, rgb(tk().ui.border));
    ctx.set_visuals(v);
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frames += 1;
        if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
            xq_tokens::toggle_mode();
            apply_visuals(&ctx);
        }
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Cjk, "1 字與輸入法");
                ui.selectable_value(&mut self.tab, Tab::Visual, "2 元件外觀");
                ui.selectable_value(&mut self.tab, Tab::Multi, "3 多視窗");
                ui.separator();
                ui.label(RichText::new(format!("縮放 {:.2}×（實體 {:.2} px/pt）・F8 深淺色", self.zoom, ctx.pixels_per_point())).color(rgb(tk().ui.text_dim)));
            });
        });
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Cjk => self.cjk(ui),
            Tab::Visual => self.visual(ui),
            Tab::Multi => self.multi(ui),
        });
        self.show_wins(&ctx);
        if self.toast.is_some_and(|t| t.elapsed().as_secs_f32() < 2.5) {
            ctx.request_repaint();
        }
    }
}

// ───────────── 1 字與輸入法 ─────────────

const SAMPLE_NAMES: &str = "台積電 鴻海 聯發科 富邦金 國泰金 長榮航 陽明 萬海 華碩 廣達";
const SAMPLE_HARD: &str = "鬱 龘 齉 爨 灣 驫 讚 寶 鑫 臺 （筆畫多的字）";
const SAMPLE_NUM: &str = "1,085.00  ▲12.50  +1.17%  ▼3.20  -0.29%  23:59:59.999";

impl Probe {
    fn cjk(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label("字型：");
            let mut src = self.font_src;
            ui.radio_value(&mut src, FontSrc::Embedded, "內嵌 Noto TC");
            ui.radio_value(&mut src, FontSrc::System, "系統字型");
            if src != self.font_src {
                self.font_src = src;
                self.font_desc = setup_fonts(&ctx, src);
            }
            if ui.checkbox(&mut self.hinting, "hinting（字貼齊像素）").changed() {
                let h = self.hinting;
                ctx.all_styles_mut(|s| s.visuals.text_options.font_hinting = h);
            }
            ui.label("縮放：");
            for z in [1.0f32, 1.25, 1.5, 2.0] {
                if ui.selectable_label(self.zoom == z, format!("{}%", (z * 100.0) as u32)).clicked() {
                    self.zoom = z;
                    ctx.set_zoom_factor(z);
                }
            }
        });
        ui.label(RichText::new(&self.font_desc).color(rgb(tk().ui.text_dim)).size(tk().font.sm));
        ui.separator();

        let t = tk();
        let sizes = [("xs", t.font.xs), ("sm", t.font.sm), ("md", t.font.md), ("lg", t.font.lg), ("xl", t.font.xl), ("", 16.0), ("", 20.0)];
        egui::Grid::new("samples").striped(false).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            for (name, px) in sizes {
                ui.label(RichText::new(format!("{px:.0}px {name}")).monospace().size(t.font.sm).color(rgb(t.ui.text_dim)));
                ui.label(RichText::new(SAMPLE_NAMES).size(px));
                ui.label(RichText::new(SAMPLE_NUM).family(FontFamily::Monospace).size(px).color(rgb(t.market.up)));
                ui.end_row();
            }
            ui.label(RichText::new("粗體 13px").size(t.font.sm).color(rgb(t.ui.text_dim)));
            ui.label(RichText::new(SAMPLE_NAMES).size(13.0).family(FontFamily::Name("bold".into())));
            ui.label(RichText::new(SAMPLE_HARD).size(13.0));
            ui.end_row();
        });
        // 淺底對照（深色主題下也畫一塊淺底，看兩種底色的字）
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().min(760.0), 54.0), Sense::hover());
        ui.painter().rect_filled(r, 0.0, Color32::from_rgb(0xf5, 0xf6, 0xf8));
        ui.painter().text(r.min + vec2(8.0, 6.0), Align2::LEFT_TOP, SAMPLE_NAMES, FontId::proportional(12.0), Color32::from_rgb(0x1a, 0x1d, 0x24));
        ui.painter().text(r.min + vec2(8.0, 28.0), Align2::LEFT_TOP, SAMPLE_NUM, FontId::monospace(12.0), Color32::from_rgb(0xd3, 0x2f, 0x2f));
        ui.separator();

        ui.label(RichText::new("輸入法").size(t.font.lg));
        ui.horizontal(|ui| {
            ui.label("搜尋框：");
            let r = ui.add(egui::TextEdit::singleline(&mut self.search).id_salt("search").hint_text("代號／股名（注音、倉頡）").desired_width(220.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.ime_log.push(format!("Enter 送出：「{}」", self.search));
            }
            if ui.button("清空").clicked() {
                self.search.clear();
                self.memo.clear();
                self.ime_log.clear();
            }
        });
        ui.horizontal(|ui| {
            ui.label("多行框：");
            ui.add(egui::TextEdit::multiline(&mut self.memo).id_salt("memo").desired_rows(3).desired_width(420.0));
        });

        // 輸入法事件與 egui 告訴系統的游標位置（系統用它擺選字窗）
        let evs: Vec<String> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Ime(ev) => Some(format!("{ev:?}")),
                    _ => None,
                })
                .collect()
        });
        let ime = ctx.output(|o| o.ime);
        let ppp = ctx.pixels_per_point();
        let inner = ctx.input(|i| i.viewport().inner_rect);
        for e in evs {
            if self.log_ime {
                eprintln!("[ime] {e}");
            }
            self.ime_log.push(e);
        }
        if let Some(ime) = ime {
            if self.last_ime_rect != Some(ime.cursor_rect) {
                self.last_ime_rect = Some(ime.cursor_rect);
                if self.log_ime {
                    // 螢幕實體像素：視窗內容左上 + 游標（點）× 縮放
                    let o = inner.map(|r| r.min).unwrap_or(egui::Pos2::ZERO);
                    let c = ime.cursor_rect;
                    eprintln!(
                        "[ime] cursor_px x={:.0} y={:.0} bottom={:.0}",
                        (o.x + c.min.x) * ppp,
                        (o.y + c.min.y) * ppp,
                        (o.y + c.max.y) * ppp
                    );
                }
            }
        }
        let n = self.ime_log.len();
        if n > 12 {
            self.ime_log.drain(..n - 12);
        }
        ui.label(RichText::new(format!("游標位置（點）：{:?}", self.last_ime_rect.map(|r| (r.min.x.round(), r.min.y.round(), r.max.y.round())))).size(t.font.sm).color(rgb(t.ui.text_dim)));
        for l in &self.ime_log {
            ui.label(RichText::new(l).monospace().size(t.font.sm).color(rgb(t.ui.text_dim)));
        }
        if self.log_ime && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))) {
            eprintln!("[ime] text search=「{}」 memo=「{}」", self.search, self.memo);
        }
    }
}

// ───────────── 2 元件外觀（只用 token） ─────────────

impl Probe {
    fn visual(&mut self, ui: &mut egui::Ui) {
        let t = tk();
        ui.label(RichText::new("只用設計 token（顏色、字級、間距、圓角、線寬）畫的元件；hover／按下／焦點／停用都在").color(rgb(t.ui.text_dim)));
        ui.add_space(t.space.md);
        ui.horizontal_top(|ui| {
            quote_card(ui, "2330", "台積電", 1085.0, 12.5, 23_456);
            ui.add_space(t.space.md);
            quote_card(ui, "2317", "鴻海", 208.5, -3.0, 51_200);
            ui.add_space(t.space.md);
            quote_card(ui, "2454", "聯發科", 1320.0, 0.0, 4_012);
        });
        ui.add_space(t.space.lg);
        self.order_ticket(ui);
        // 淡入淡出提示（動效時間 0.2 秒，用 egui 內建的 animate_value）
        if let Some(t0) = self.toast {
            let age = t0.elapsed().as_secs_f32();
            let target = if age < 2.0 { 1.0 } else { 0.0 };
            let a = ui.ctx().animate_value_with_time(egui::Id::unique("toast"), target, 0.2);
            if a > 0.01 {
                let screen = ui.ctx().content_rect();
                let r = Rect::from_min_size(pos2(screen.max.x - 280.0, screen.max.y - 64.0 + (1.0 - a) * 12.0), vec2(260.0, 40.0));
                let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("toast-layer")));
                p.add(egui::epaint::Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha((90.0 * a) as u8) }.as_shape(r, CornerRadius::same(t.radius.md as u8)));
                p.rect(r, t.radius.md, rgb(t.ui.panel).gamma_multiply(a), Stroke::new(t.stroke.hairline, rgb(t.ui.border).gamma_multiply(a)), StrokeKind::Inside);
                p.text(r.left_center() + vec2(t.space.md, 0.0), Align2::LEFT_CENTER, "委託已送出（示意）", FontId::proportional(t.font.md), rgb(t.ui.text).gamma_multiply(a));
            } else if age > 2.5 {
                self.toast = None;
            }
        }
    }

    fn order_ticket(&mut self, ui: &mut egui::Ui) {
        let t = tk();
        egui::Frame::new()
            .fill(rgb(t.ui.panel))
            .stroke(Stroke::new(t.stroke.hairline, rgb(t.ui.border)))
            .corner_radius(t.radius.md)
            .inner_margin(t.space.md)
            .show(ui, |ui| {
                ui.set_width(360.0);
                ui.label(RichText::new("下單匣（示意）").size(t.font.lg));
                ui.add_space(t.space.sm);
                ui.horizontal(|ui| {
                    for (buy, label, col) in [(true, "買進", t.market.up), (false, "賣出", t.market.down)] {
                        let on = self.side_buy == buy;
                        let b = egui::Button::new(RichText::new(label).color(if on { Color32::WHITE } else { rgb(col) }))
                            .fill(if on { rgb(col) } else { rgb(t.ui.bg) })
                            .stroke(Stroke::new(t.stroke.hairline, rgb(col)))
                            .corner_radius(t.radius.sm)
                            .min_size(vec2(80.0, 26.0));
                        if ui.add(b).clicked() {
                            self.side_buy = buy;
                        }
                    }
                });
                ui.add_space(t.space.sm);
                egui::Grid::new("ticket").spacing(vec2(t.space.md, t.space.sm)).show(ui, |ui| {
                    ui.label(RichText::new("價格").color(rgb(t.ui.text_dim)));
                    ui.add_sized([160.0, t.layout.table_row], egui::TextEdit::singleline(&mut self.price).font(FontId::monospace(t.font.md)));
                    ui.end_row();
                    ui.label(RichText::new("張數").color(rgb(t.ui.text_dim)));
                    ui.add_sized([160.0, t.layout.table_row], egui::TextEdit::singleline(&mut self.qty).font(FontId::monospace(t.font.md)));
                    ui.end_row();
                });
                ui.add_space(t.space.sm);
                ui.horizontal(|ui| {
                    let col = if self.side_buy { t.market.up } else { t.market.down };
                    let send = egui::Button::new(RichText::new("送出委託").color(Color32::WHITE).size(t.font.lg))
                        .fill(rgb(col))
                        .corner_radius(t.radius.sm)
                        .min_size(vec2(160.0, 30.0));
                    if ui.add(send).clicked() {
                        self.toast = Some(Instant::now());
                    }
                    ui.add_enabled(false, egui::Button::new("刪單（停用示意）").corner_radius(t.radius.sm).min_size(vec2(120.0, 30.0)));
                });
            });
    }
}

fn quote_card(ui: &mut egui::Ui, sym: &str, name: &str, price: f64, chg: f64, vol: i64) {
    let t = tk();
    let (r, resp) = ui.allocate_exact_size(vec2(220.0, 100.0), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let col = if chg > 0.0 { t.market.up } else if chg < 0.0 { t.market.down } else { t.market.flat };
    let p = ui.painter();
    let border = lerp_color(rgb(t.ui.border), rgb(t.ui.accent), hover);
    p.rect(r, t.radius.md, rgb(t.ui.panel), Stroke::new(t.stroke.hairline, border), StrokeKind::Inside);
    if resp.has_focus() {
        p.rect_stroke(r.expand(2.0), t.radius.md + 2.0, Stroke::new(t.stroke.selected, rgb(t.ui.accent)), StrokeKind::Outside);
    }
    let m = t.space.md;
    p.text(r.min + vec2(m, m), Align2::LEFT_TOP, name, FontId::proportional(t.font.xl), rgb(t.ui.text));
    p.text(pos2(r.max.x - m, r.min.y + m + 2.0), Align2::RIGHT_TOP, sym, FontId::monospace(t.font.sm), rgb(t.ui.text_dim));
    p.text(pos2(r.min.x + m, r.center().y + 2.0), Align2::LEFT_CENTER, format!("{price:.2}"), FontId::monospace(20.0), rgb(col));
    let arrow = if chg > 0.0 { "▲" } else if chg < 0.0 { "▼" } else { "－" };
    p.text(
        pos2(r.max.x - m, r.center().y + 2.0),
        Align2::RIGHT_CENTER,
        format!("{arrow}{:.2} {:+.2}%", chg.abs(), chg / (price - chg) * 100.0),
        FontId::monospace(t.font.md),
        rgb(col),
    );
    p.text(pos2(r.min.x + m, r.max.y - m), Align2::LEFT_BOTTOM, format!("量 {vol}"), FontId::monospace(t.font.sm), rgb(t.ui.text_dim));
    // 迷你走勢（細線＋半透明面積），示意 token 的 area_fill
    let pts: Vec<egui::Pos2> = (0..40)
        .map(|i| {
            let x = r.max.x - m - 70.0 + i as f32 * 70.0 / 39.0;
            let y = r.max.y - m - ((i as f32 * 0.37 + sym.len() as f32).sin() * 0.5 + 0.5) * 12.0;
            pos2(x, y)
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(t.stroke.series, rgb(col))));
    resp.on_hover_text(format!("{sym} {name}：點兩下開走勢圖（示意）"));
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

// ───────────── 3 多視窗 ─────────────

impl Probe {
    fn multi(&mut self, ui: &mut egui::Ui) {
        let t = tk();
        ui.horizontal(|ui| {
            if ui.button("開新視窗").clicked() {
                self.open_win(None);
            }
            if ui.button("開 4 個").clicked() {
                for _ in 0..4 {
                    self.open_win(None);
                }
            }
            if ui.button("全部關閉").clicked() {
                for w in &mut self.wins {
                    w.want_close = true;
                }
            }
            ui.label(format!("目前 {} 個子視窗；主視窗第 {} 幀", self.wins.len(), self.frames));
        });
        ui.add_space(t.space.md);
        ui.group(|ui| match self.main_panel.as_mut() {
            Some(p) => {
                panel_ui(ui, p);
                if ui.button("把這個面板移到新視窗").clicked() {
                    let p = self.main_panel.take();
                    self.open_win(p);
                }
            }
            None => {
                ui.label("面板在別的視窗裡；在那個視窗按「移回主視窗」");
            }
        });
        ui.add_space(t.space.md);
        for w in &self.wins {
            ui.label(
                RichText::new(format!(
                    "視窗 {}：{} 幀，第一幀 {}",
                    w.n,
                    w.frames,
                    w.first_frame_ms.map(|m| format!("{m:.0} ms")).unwrap_or_else(|| "—".into())
                ))
                .monospace()
                .size(t.font.sm),
            );
        }
        edge_widgets(ui);
    }

    fn show_wins(&mut self, ctx: &egui::Context) {
        let mut back: Option<PanelState> = None;
        for w in self.wins.iter_mut() {
            let b = egui::ViewportBuilder::default().with_inner_size([520.0, 360.0]).with_title(format!("子視窗 {}（egui）", w.n));
            ctx.show_viewport_immediate(w.id, b, |ui, _class| {
                w.frames += 1;
                if w.first_frame_ms.is_none() {
                    let ms = w.opened.elapsed().as_secs_f64() * 1000.0;
                    w.first_frame_ms = Some(ms);
                    eprintln!("[mw] first-frame {} {ms:.1}ms", w.n);
                }
                let (close_req, focused) = ui.ctx().input(|i| (i.viewport().close_requested(), i.viewport().focused.unwrap_or(false)));
                // 快捷鍵要用 consume_shortcut（看按下那一刻的修飾鍵）：視窗閒置時按下／放開會擠在同一幀，
                // 用 i.modifiers（幀尾狀態）＋key_pressed 會漏掉（雲端實測踩到）
                let ctrl_w = ui.ctx().input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::W)));
                if ctrl_w {
                    eprintln!("[mw] ctrl+w {} focused={focused}", w.n);
                }
                if std::env::var("XQ_PROBE_DEBUG").is_ok() {
                    ui.ctx().input(|i| {
                        for e in &i.events {
                            eprintln!("[mw] ev {} {e:?}", w.n);
                        }
                    });
                }
                if close_req || (focused && ctrl_w) {
                    // 只關這個視窗：不要讓 eframe 關（CancelClose），自己從清單拿掉
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    w.want_close = true;
                }
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("子視窗 {}", w.n)).size(tk().font.lg));
                        ui.label(format!("第 {} 幀・{}", w.frames, if focused { "有焦點" } else { "沒焦點" }));
                    });
                    ui.horizontal(|ui| {
                        if ui.button("關閉這個視窗（或 Ctrl+W）").clicked() {
                            w.want_close = true;
                        }
                        if ui.button("開對話框").clicked() {
                            w.modal = true;
                        }
                    });
                    if let Some(p) = w.panel.as_mut() {
                        ui.group(|ui| {
                            panel_ui(ui, p);
                        });
                        if ui.button("移回主視窗").clicked() {
                            back = w.panel.take();
                        }
                    }
                    edge_widgets(ui);
                });
                if w.modal {
                    let m = egui::Modal::new(egui::Id::unique(("modal", w.n))).show(ui.ctx(), |ui| {
                        ui.label("這是只屬於這個視窗的對話框");
                        if ui.button("確定").clicked() {
                            w.modal = false;
                        }
                    });
                    if m.should_close() {
                        w.modal = false;
                    }
                }
            });
        }
        if back.is_some() {
            self.main_panel = back;
        }
        self.wins.retain(|w| {
            if w.want_close {
                eprintln!("[mw] close {} frames={}", w.n, w.frames);
            }
            !w.want_close
        });
    }
}

/// 放在視窗右下角的選單、下拉、提示：看會不會被視窗邊緣切掉（egui 的浮層畫在視窗裡，不是獨立 OS 視窗）
fn edge_widgets(ui: &mut egui::Ui) {
    let r = ui.max_rect();
    let at = Rect::from_min_size(pos2(r.max.x - 150.0, r.max.y - 30.0), vec2(150.0, 26.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(at), |ui| {
        ui.horizontal(|ui| {
            ui.menu_button("選單 ▾", |ui| {
                for i in 1..=12 {
                    let _ = ui.button(format!("選項 {i}：很長很長的選單文字"));
                }
            });
            ui.label("提示?").on_hover_text("這是一段很長的提示文字，放在視窗右下角時看會不會被切掉或自動往內移。");
        });
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let renderer = match env("XQ_RENDERER").as_deref() {
        Some("wgpu") => eframe::Renderer::Wgpu,
        _ => eframe::Renderer::Glow,
    };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 760.0]).with_title("egui 弱項驗證（probe）"),
        renderer,
        ..Default::default()
    };
    eframe::run_native("egui-probe", opts, Box::new(|cc| Ok(Box::new(Probe::new(cc)))))
}

#[cfg(target_arch = "wasm32")]
fn main() {}
