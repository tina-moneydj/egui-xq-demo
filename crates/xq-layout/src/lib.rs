//! 伺服器下發版面（第 2 層）：伺服器送一份 JSON，決定頂部選單、有哪些頁、每頁擺哪些內建元件
//! （走勢圖／報價表／資訊格）、怎麼分割、報價表顯示哪些欄位。高頻元件本身仍內建在程式裡，JSON 只是「擺法」。
//!
//! 這個 crate 只做純邏輯（不依賴 egui）：解析 → 驗證（上限＋降級）→ 分割計算。
//! - 壞 JSON、沒有任何頁：回 `Err`，呼叫端沿用上一份好的版面。
//! - 認不得的元件、欄位、週期：不當掉，降級成「不支援」佔位格／略過，並記一條警告（舊版程式收到新版 JSON 也能開）。
//! - 所有清單都有上限（頁數、層數、每頁格數、走勢圖數、代號數、選單項目），超過的截掉並警告。
//!
//! 格式說明見 `docs/server-layout.md`，範例 `layouts/demo.json`。

use std::collections::HashMap;

use serde::Deserialize;

/// 目前支援的格式版本；比這新的版本照樣盡量解析（認不得的欄位忽略），並警告
pub const VERSION: u32 = 1;
/// 單份 JSON 最大位元組（抓取端也用這個上限，超過不讀）
pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_PAGES: usize = 32;
/// 分割巢狀層數上限
pub const MAX_DEPTH: usize = 6;
/// 每頁元件格數上限
pub const MAX_PANES: usize = 16;
/// 每頁走勢圖上限（每張圖有自己的三層快取，記憶體預算）
pub const MAX_CHARTS: usize = 6;
/// 每張報價表代號上限
pub const MAX_SYMBOLS: usize = 300;
/// 選單項目總數上限（含分組、分隔線）
pub const MAX_MENU_ITEMS: usize = 64;
/// 選單巢狀層數上限（頂層算第 1 層）
pub const MAX_MENU_DEPTH: usize = 3;
/// 標籤字數上限（超過截斷）
pub const MAX_LABEL: usize = 40;

/// 報價表可選欄位（順序＝預設順序）；`Pane::Quotes::columns` 存的是這裡的索引
pub const COLUMNS: [&str; 6] = ["symbol", "name", "price", "change", "pct", "volume"];

/// 同一份 JSON 給桌面、網頁、手機吃：頁和選單項目可用 `platforms` 限定平台，頁可用 `roots` 給各平台不同擺法
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Desktop,
    Web,
    Mobile,
}

impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Platform::Desktop => "desktop",
            Platform::Web => "web",
            Platform::Mobile => "mobile",
        }
    }
}

/// 空清單＝所有平台
fn for_platform(list: &[String], p: Platform) -> bool {
    list.is_empty() || list.iter().any(|x| x.trim().eq_ignore_ascii_case(p.name()))
}

// ───────────────────────── 驗證後的結果 ─────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub version: u32,
    pub title: String,
    pub menu: Vec<MenuEntry>,
    pub pages: Vec<Page>,
    /// 起始頁（`pages` 的索引）
    pub start: usize,
    /// 降級／截斷的說明（顯示在狀態列，方便伺服器端除錯）
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MenuEntry {
    Group { label: String, items: Vec<MenuEntry> },
    Item { label: String, action: Action },
    Separator,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// 切到某頁（`pages` 的索引）
    Page(usize),
    /// 連動的走勢圖／資訊格換代號
    Symbol(String),
    /// 用系統瀏覽器開網址（第 1 層：低頻頁面交給網頁）
    Url(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub root: Node,
    /// 依深度優先順序；`Node::Leaf` 存的是這裡的索引
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// 左右並排
    Row,
    /// 上下堆疊
    Col,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Split { dir: Dir, sizes: Vec<f32>, children: Vec<Node> },
    Leaf(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pane {
    /// `period`：T 分時／D 日K／W 週K／M 月K。`symbol` 空＝用連動代號。
    /// `link`（預設 true）：點報價表／選單換代號時跟著換；false＝固定看 `symbol`
    Chart { symbol: String, period: String, link: bool },
    /// `symbols`：（代號, 名稱）；名稱可空，由程式用內建清單或行情補
    Quotes { title: String, symbols: Vec<(String, String)>, columns: Vec<usize> },
    Info,
    /// 認不得或超出上限的元件：畫一個說明格，不當掉
    Unsupported(String),
}

// ───────────────────────── 原始 JSON（全部寬鬆、可缺） ─────────────────────────

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawScreen {
    version: u32,
    title: String,
    start: String,
    menu: Vec<RawMenu>,
    pages: Vec<RawPage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMenu {
    label: String,
    page: Option<String>,
    symbol: Option<String>,
    url: Option<String>,
    separator: bool,
    items: Vec<RawMenu>,
    platforms: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPage {
    id: String,
    title: String,
    root: Option<RawNode>,
    /// 各平台各自的擺法（"desktop"／"web"／"mobile"），沒有就用 `root`
    roots: HashMap<String, RawNode>,
    platforms: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawNode {
    split: Option<String>,
    sizes: Vec<f32>,
    children: Vec<RawNode>,
    widget: Option<String>,
    title: Option<String>,
    symbol: Option<String>,
    period: Option<String>,
    link: Option<bool>,
    symbols: Vec<RawSym>,
    columns: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSym {
    Code(String),
    Full {
        symbol: String,
        #[serde(default)]
        name: String,
    },
}

// ───────────────────────── 解析＋驗證 ─────────────────────────

fn label(s: &str) -> String {
    s.trim().chars().take(MAX_LABEL).collect()
}

/// 解析並驗證一份版面 JSON（桌面版）。壞檔回 `Err`（呼叫端沿用上一份）；其他問題降級並寫進 `warnings`。
pub fn parse(json: &[u8]) -> Result<Screen, String> {
    parse_for(json, Platform::Desktop)
}

/// 同 `parse`，但挑出某個平台的頁、選單與擺法
pub fn parse_for(json: &[u8], platform: Platform) -> Result<Screen, String> {
    if json.len() > MAX_BYTES {
        return Err(format!("版面 JSON 太大（{} KB，上限 {} KB）", json.len() / 1024, MAX_BYTES / 1024));
    }
    let raw: RawScreen = serde_json::from_slice(json).map_err(|e| format!("版面 JSON 格式錯誤：{e}"))?;
    let mut w = Vec::new();
    if raw.version > VERSION {
        w.push(format!("版面版本 {} 比程式支援的 {VERSION} 新，未知欄位已忽略", raw.version));
    }

    // 頁：先收 id（選單要用），重複 id 只留第一個
    let mut pages: Vec<Page> = Vec::new();
    // 刻意不給這個平台的頁：指向它們的選單項目安靜略過，不算錯
    let mut other: Vec<String> = Vec::new();
    for (k, mut rp) in raw.pages.into_iter().enumerate() {
        if !for_platform(&rp.platforms, platform) {
            other.push(rp.id.trim().to_string());
            continue;
        }
        if pages.len() >= MAX_PAGES {
            w.push(format!("頁數超過 {MAX_PAGES}，後面的頁已略過"));
            break;
        }
        let id = if rp.id.trim().is_empty() { format!("page{}", k + 1) } else { rp.id.trim().to_string() };
        if pages.iter().any(|p| p.id == id) {
            w.push(format!("頁 id「{id}」重複，只用第一個"));
            continue;
        }
        let title = if rp.title.trim().is_empty() { id.clone() } else { label(&rp.title) };
        let Some(root) = rp.roots.remove(platform.name()).or(rp.root) else {
            w.push(format!("頁「{id}」沒有 root，已略過"));
            continue;
        };
        let mut panes = Vec::new();
        let mut charts = 0;
        let root = node(root, 1, &id, &mut panes, &mut charts, &mut w);
        pages.push(Page { id, title, root, panes });
    }
    if pages.is_empty() {
        return Err("版面沒有任何可用的頁".into());
    }
    let start = match raw.start.trim() {
        "" => 0,
        s => pages.iter().position(|p| p.id == s).unwrap_or_else(|| {
            w.push(format!("起始頁「{s}」不存在，改用第一頁"));
            0
        }),
    };
    let mut count = 0;
    let menu = menu(raw.menu, 1, &pages, &other, platform, &mut count, &mut w);
    Ok(Screen {
        version: raw.version,
        title: label(&raw.title),
        menu,
        pages,
        start,
        warnings: w,
    })
}

fn node(r: RawNode, depth: usize, page: &str, panes: &mut Vec<Pane>, charts: &mut usize, w: &mut Vec<String>) -> Node {
    if let Some(dir) = r.split.as_deref() {
        let dir = match dir {
            "row" | "h" | "horizontal" => Dir::Row,
            "col" | "column" | "v" | "vertical" => Dir::Col,
            other => {
                w.push(format!("頁「{page}」：分割方向「{other}」不認得，當作 col"));
                Dir::Col
            }
        };
        if depth >= MAX_DEPTH {
            w.push(format!("頁「{page}」：分割超過 {MAX_DEPTH} 層，深處改成佔位格"));
            return leaf(Pane::Unsupported("分割太深".into()), page, panes, w);
        }
        if r.children.is_empty() {
            w.push(format!("頁「{page}」：分割沒有 children"));
            return leaf(Pane::Unsupported("空的分割".into()), page, panes, w);
        }
        let n = r.children.len();
        let sizes = norm_sizes(&r.sizes, n);
        let children = r.children.into_iter().map(|c| node(c, depth + 1, page, panes, charts, w)).collect();
        return Node::Split { dir, sizes, children };
    }
    let widget = r.widget.as_deref().unwrap_or("").trim();
    let pane = match widget {
        "chart" => {
            if *charts >= MAX_CHARTS {
                w.push(format!("頁「{page}」：走勢圖超過 {MAX_CHARTS} 張，多的改成佔位格"));
                Pane::Unsupported(format!("走勢圖上限 {MAX_CHARTS} 張"))
            } else {
                *charts += 1;
                let period = r.period.as_deref().map(str::trim).unwrap_or("T").to_ascii_uppercase();
                let period = if ["T", "D", "W", "M"].contains(&period.as_str()) {
                    period
                } else {
                    w.push(format!("頁「{page}」：週期「{period}」不認得，改用 T"));
                    "T".into()
                };
                Pane::Chart { symbol: r.symbol.map(|s| s.trim().to_string()).unwrap_or_default(), period, link: r.link.unwrap_or(true) }
            }
        }
        "quotes" => {
            let mut symbols = Vec::new();
            for s in r.symbols {
                let (sym, name) = match s {
                    RawSym::Code(c) => (c, String::new()),
                    RawSym::Full { symbol, name } => (symbol, name),
                };
                let sym = sym.trim().to_string();
                if sym.is_empty() || symbols.iter().any(|(s, _): &(String, String)| *s == sym) {
                    continue;
                }
                if symbols.len() >= MAX_SYMBOLS {
                    w.push(format!("頁「{page}」：報價表代號超過 {MAX_SYMBOLS} 檔，多的已略過"));
                    break;
                }
                symbols.push((sym, label(&name)));
            }
            let mut columns = Vec::new();
            for c in &r.columns {
                match COLUMNS.iter().position(|k| k == c) {
                    Some(i) if !columns.contains(&i) => columns.push(i),
                    Some(_) => {}
                    None => w.push(format!("頁「{page}」：報價表欄位「{c}」不認得，已略過")),
                }
            }
            if columns.is_empty() {
                columns = (0..COLUMNS.len()).collect();
            }
            Pane::Quotes { title: label(r.title.as_deref().unwrap_or("報價")), symbols, columns }
        }
        "info" => Pane::Info,
        "" => {
            w.push(format!("頁「{page}」：有一格沒有 widget 也不是 split"));
            Pane::Unsupported("空白格".into())
        }
        other => {
            w.push(format!("頁「{page}」：元件「{other}」這版程式不支援，顯示佔位格"));
            Pane::Unsupported(format!("不支援的元件：{}", label(other)))
        }
    };
    leaf(pane, page, panes, w)
}

fn leaf(p: Pane, page: &str, panes: &mut Vec<Pane>, w: &mut Vec<String>) -> Node {
    if panes.len() >= MAX_PANES {
        // 超過上限：沿用最後一格的索引會重複畫，改指向一個共用的佔位格
        if !matches!(panes.last(), Some(Pane::Unsupported(s)) if s.starts_with("格數上限")) {
            w.push(format!("頁「{page}」：元件超過 {MAX_PANES} 格，多的不顯示"));
            if let Some(last) = panes.last_mut() {
                *last = Pane::Unsupported(format!("格數上限 {MAX_PANES}"));
            }
        }
        return Node::Leaf(panes.len().saturating_sub(1));
    }
    panes.push(p);
    Node::Leaf(panes.len() - 1)
}

/// 比例正規化：缺的、非正數、NaN 補平均值，總和 = 1
fn norm_sizes(raw: &[f32], n: usize) -> Vec<f32> {
    let valid: Vec<f32> = raw.iter().copied().filter(|v| v.is_finite() && *v > 0.0).collect();
    let fill = if valid.is_empty() { 1.0 } else { valid.iter().sum::<f32>() / valid.len() as f32 };
    let mut s: Vec<f32> = (0..n).map(|i| raw.get(i).copied().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(fill)).collect();
    let sum: f32 = s.iter().sum();
    for v in &mut s {
        *v /= sum;
    }
    s
}

fn menu(raw: Vec<RawMenu>, depth: usize, pages: &[Page], other: &[String], platform: Platform, count: &mut usize, w: &mut Vec<String>) -> Vec<MenuEntry> {
    let mut out = Vec::new();
    for m in raw {
        if !for_platform(&m.platforms, platform) || m.page.as_deref().is_some_and(|p| other.iter().any(|o| o == p.trim())) {
            continue;
        }
        if *count >= MAX_MENU_ITEMS {
            w.push(format!("選單項目超過 {MAX_MENU_ITEMS} 個，後面的已略過"));
            break;
        }
        *count += 1;
        if m.separator {
            out.push(MenuEntry::Separator);
            continue;
        }
        let lb = label(&m.label);
        if !m.items.is_empty() {
            if depth >= MAX_MENU_DEPTH {
                w.push(format!("選單「{lb}」超過 {MAX_MENU_DEPTH} 層，子選單已略過"));
                continue;
            }
            let items = menu(m.items, depth + 1, pages, other, platform, count, w);
            // 子項目全被略過（例如都不給這個平台）就不顯示空的分組
            if items.is_empty() {
                continue;
            }
            out.push(MenuEntry::Group { label: lb, items });
            continue;
        }
        let action = if let Some(p) = m.page.as_deref() {
            match pages.iter().position(|x| x.id == p.trim()) {
                Some(i) => Action::Page(i),
                None => {
                    w.push(format!("選單「{lb}」指向不存在的頁「{p}」，已略過"));
                    continue;
                }
            }
        } else if let Some(s) = m.symbol.filter(|s| !s.trim().is_empty()) {
            Action::Symbol(s.trim().to_string())
        } else if let Some(u) = m.url {
            // 只開 http(s)，不讓伺服器設定叫起任意協定
            if u.starts_with("https://") || u.starts_with("http://") {
                Action::Url(u)
            } else {
                w.push(format!("選單「{lb}」的網址不是 http(s)，已略過"));
                continue;
            }
        } else {
            w.push(format!("選單「{lb}」沒有 page／symbol／url，已略過"));
            continue;
        };
        if lb.is_empty() {
            w.push("有一個選單項目沒有 label，已略過".into());
            continue;
        }
        out.push(MenuEntry::Item { label: lb, action });
    }
    out
}

// ───────────────────────── 分割計算 ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
}

/// 把一頁的分割樹攤成矩形。`panes[i]` 是第 i 格的位置（沒出現在樹裡的格 w=h=0），
/// `gaps` 是格與格之間的分隔線。輸出 Vec 由呼叫端重用（每幀不配置）。像素取整避免文字模糊。
pub fn solve(node: &Node, r: Rect, gap: f32, panes: &mut Vec<Rect>, gaps: &mut Vec<Rect>) {
    panes.clear();
    gaps.clear();
    walk(node, r, gap, panes, gaps);
}

fn walk(node: &Node, r: Rect, gap: f32, panes: &mut Vec<Rect>, gaps: &mut Vec<Rect>) {
    match node {
        Node::Leaf(i) => {
            if panes.len() <= *i {
                panes.resize(*i + 1, Rect::default());
            }
            panes[*i] = r;
        }
        Node::Split { dir, sizes, children } => {
            let n = children.len();
            let total = if *dir == Dir::Row { r.w } else { r.h };
            let avail = (total - gap * n.saturating_sub(1) as f32).max(0.0);
            let base = if *dir == Dir::Row { r.x } else { r.y };
            let end = base + total;
            let mut pos = base;
            let mut acc = 0.0;
            for (k, c) in children.iter().enumerate() {
                acc += sizes.get(k).copied().unwrap_or(0.0);
                // 最後一格吃到底，避免累積誤差留縫
                let next = if k + 1 == n { end } else { (base + avail * acc + gap * k as f32).round().clamp(pos, end) };
                let len = (next - pos).max(0.0);
                let cr = if *dir == Dir::Row { Rect::new(pos, r.y, len, r.h) } else { Rect::new(r.x, pos, r.w, len) };
                walk(c, cr, gap, panes, gaps);
                pos = next;
                if k + 1 < n {
                    gaps.push(if *dir == Dir::Row { Rect::new(pos, r.y, gap, r.h) } else { Rect::new(r.x, pos, r.w, gap) });
                    pos += gap;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_normalize() {
        assert_eq!(norm_sizes(&[3.0, 1.0], 2), vec![0.75, 0.25]);
        assert_eq!(norm_sizes(&[], 4), vec![0.25; 4]);
        // 缺的補平均、負數／NaN 當缺
        let s = norm_sizes(&[2.0, -1.0, f32::NAN], 3);
        assert!((s.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(s.iter().all(|v| (*v - 1.0 / 3.0).abs() < 1e-6));
    }
}
